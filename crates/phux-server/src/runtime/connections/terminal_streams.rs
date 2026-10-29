use super::dispatch::{ConnectionEnding, FrameFlow};
use super::hello::NegotiatedConnection;
use super::plumbing::{ClientPlumbing, ConnectionClose};
use crate::runtime::input_lane::{InputLaneHandle, RoutedInput};
use crate::runtime::{
    bootstrap_attach_terminal, handle_detach_terminal, handle_terminal_input,
    subscribe_attach_terminal,
};
use crate::state::{ClientId, SharedState, TerminalInput};
use crate::transport::quic::{
    QuicStreamEvent, QuicStreamFailure, QuicWriter, pump_terminal_stream, refuse_terminal_stream,
};
use phux_dial::window::SendWindow;
use phux_protocol::ids::{ResourceId as WireResourceId, StreamId};
use phux_protocol::wire::frame::{DetachReason, ErrorCode, FrameKind};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// A frame kind a client may not send in the negotiated phase: close.
pub(super) fn direction_invalid(client_id: ClientId, other: &FrameKind) -> FrameFlow {
    warn!(?client_id, kind = ?other, "direction-invalid client frame; closing");
    FrameFlow::Break(ConnectionEnding::Violation(ConnectionClose {
        attached_reason: Some("direction-invalid frame"),
        detach_reason: DetachReason::ProtocolError,
        code: ErrorCode::InvalidCommand,
        message: format!("frame is not valid from a client in the negotiated phase: {other:?}"),
    }))
}

/// The fields of one `ATTACH` frame.
pub(super) struct AttachFrame {
    pub(super) attach_id: u32,
    pub(super) target: phux_protocol::wire::frame::AttachTarget,
    pub(super) viewport: phux_protocol::wire::frame::ViewportInfo,
    pub(super) request_scrollback: bool,
    pub(super) scrollback_limit_lines: u32,
    pub(super) role_policy: Option<phux_protocol::wire::frame::RolePolicy>,
}

/// `INPUT_*` frames: routed to the Terminal's input path.
pub(super) const fn is_input_frame(frame: &FrameKind) -> bool {
    matches!(
        frame,
        FrameKind::InputKey { .. }
            | FrameKind::InputMouse { .. }
            | FrameKind::InputFocus { .. }
            | FrameKind::InputPaste { .. }
            | FrameKind::InputTerminalReply { .. }
    )
}

/// Frames about an attached Terminal's size or content stream.
pub(super) const fn is_stream_frame(frame: &FrameKind) -> bool {
    matches!(
        frame,
        FrameKind::ViewportResize { .. }
            | FrameKind::ResizeTerminal { .. }
            | FrameKind::FrameAck { .. }
            | FrameKind::HistoryRequest { .. }
    )
}

/// L3 metadata and event-subscription frames.
pub(super) const fn is_metadata_frame(frame: &FrameKind) -> bool {
    matches!(
        frame,
        FrameKind::GetMetadata { .. }
            | FrameKind::SetMetadata { .. }
            | FrameKind::DeleteMetadata { .. }
            | FrameKind::ListMetadata { .. }
            | FrameKind::SubscribeMetadata { .. }
            | FrameKind::SubscribeEvents { .. }
    )
}

/// Handle one QUIC Terminal-stream event (docs/protocol.md#quic-stream-routing). `Bound` binds and
/// bootstraps the stream; `Ended` detaches as `DETACH_RESOURCE` would,
/// ignoring a stale generation.
pub(super) async fn handle_stream_event(
    state: &SharedState,
    client_id: ClientId,
    event: QuicStreamEvent,
    plumbing: &mut ClientPlumbing,
    negotiated: Option<&NegotiatedConnection>,
    token: &CancellationToken,
) {
    // A bind before HELLO or without the negotiated shape is reset.
    let Some(selection) = negotiated.filter(|selection| selection.quic_streams()) else {
        if let QuicStreamEvent::Bound { send, recv, .. } = event {
            refuse_terminal_stream(send, recv);
        }
        return;
    };
    match event {
        QuicStreamEvent::Bound {
            terminal_id,
            stream_id,
            send,
            recv,
            window,
            frames,
            frame_bytes,
            terminal_frame_bytes,
        } => {
            bind_terminal_stream(
                state,
                client_id,
                terminal_id,
                stream_id,
                send,
                recv,
                window,
                frames,
                frame_bytes,
                terminal_frame_bytes,
                plumbing,
                selection,
                token,
            )
            .await;
        }
        QuicStreamEvent::Ended {
            terminal_id,
            stream_id,
        } => {
            if plumbing.is_current_stream(&terminal_id, stream_id) {
                teardown_terminal_stream(state, client_id, plumbing, &terminal_id).await;
            } else {
                debug!(
                    ?client_id,
                    ?terminal_id,
                    "stale terminal-stream end ignored"
                );
            }
        }
        QuicStreamEvent::Failed {
            terminal_id,
            stream_id,
            failure,
        } => {
            handle_stream_failure(state, client_id, plumbing, terminal_id, stream_id, failure)
                .await;
        }
    }
}

async fn handle_stream_failure(
    state: &SharedState,
    client_id: ClientId,
    plumbing: &mut ClientPlumbing,
    terminal_id: WireResourceId,
    stream_id: StreamId,
    failure: QuicStreamFailure,
) {
    if !plumbing.is_current_stream(&terminal_id, stream_id) {
        debug!(
            ?client_id,
            ?terminal_id,
            "stale terminal-stream failure ignored"
        );
        return;
    }
    let (code, message) = match failure {
        QuicStreamFailure::Framing(framing) => (ErrorCode::FrameTooLarge, framing.wire_message()),
        QuicStreamFailure::IncompleteFrame => (
            ErrorCode::MalformedMessage,
            "Terminal QUIC stream ended or timed out mid-frame".to_owned(),
        ),
        QuicStreamFailure::Transport(message) => (
            ErrorCode::MalformedMessage,
            format!("Terminal QUIC stream failed: {message}"),
        ),
    };
    crate::runtime::send_error(
        &plumbing.out_tx,
        code,
        &format!("{terminal_id:?} stream {stream_id:?}: {message}"),
    )
    .await;
    teardown_terminal_stream(state, client_id, plumbing, &terminal_id).await;
}

/// Subscribe-and-bootstrap one bound Terminal stream. The subscription
/// registers the control mailbox; the bootstrap runs on the stream's. A
/// failed bootstrap keeps the subscription (the client may re-bind) but
/// finishes the stream and reports the refusal on control.
#[allow(
    clippy::too_many_arguments,
    reason = "the negotiated connection context plus the split stream halves; same list as the attach path"
)]
async fn bind_terminal_stream(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: WireResourceId,
    stream_id: StreamId,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    window: SendWindow,
    frames: tokio::sync::mpsc::Sender<crate::transport::quic::AdmittedFrame>,
    frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
    terminal_frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
    plumbing: &mut ClientPlumbing,
    selection: &NegotiatedConnection,
    token: &CancellationToken,
) {
    // workload-auth §6: OBSERVE on the bound Terminal, before membership is
    // consulted, so an unauthorized bind learns nothing about the Terminal.
    if crate::runtime::dispatch_guard::refuse_stream_bind(
        state,
        client_id,
        &terminal_id,
        &plumbing.out_tx,
    )
    .await
    {
        refuse_terminal_stream(send, recv);
        return;
    }
    // Authorization is subscription membership.
    let subscribed = state.with(|s| {
        let local = match s.resolve_resource(&terminal_id).into_owned() {
            crate::state::ResolvedOwned::Local(local) => local,
            crate::state::ResolvedOwned::Remote(_) | crate::state::ResolvedOwned::Unknown => {
                return false;
            }
        };
        s.subscribers_for_terminal(local.id).contains(&client_id)
    });
    if !subscribed {
        refuse_terminal_stream(send, recv);
        crate::runtime::send_error(
            &plumbing.out_tx,
            ErrorCode::TerminalNotFound,
            &format!("STREAM_BIND for unsubscribed terminal: {terminal_id}"),
        )
        .await;
        return;
    }
    let writer = QuicWriter::from_terminal_stream(send, window);
    let diagnostics = writer.diagnostic_tracker();
    debug!(?client_id, ?stream_id, context = ?diagnostics.context(), "bound QUIC diagnostic stream");
    let bind_started = std::time::Instant::now();
    let (stream_tx, ingress_active, ingress_cancel) = plumbing
        .bind_stream(client_id, terminal_id.clone(), stream_id, writer)
        .await;
    // Subscribe against control before bootstrapping, so lifecycle fanout
    // resolves there even if the Terminal dies mid-bootstrap. The role was
    // declared by the `ATTACH_RESOURCE` (ADR-0127).
    let subscription =
        subscribe_attach_terminal(state, client_id, &terminal_id, &plumbing.out_tx, None);
    let Ok(crate::runtime::commands::AttachSubscription { core, handle, .. }) = subscription else {
        let mut recv = recv;
        let _ = recv.stop(0x10_u32.into());
        plumbing.drop_stream_binding(&terminal_id).await;
        crate::runtime::send_error(
            &plumbing.out_tx,
            ErrorCode::TerminalNotFound,
            &format!("STREAM_BIND raced terminal death: {terminal_id}"),
        )
        .await;
        return;
    };
    tokio::spawn(pump_terminal_stream(
        recv,
        terminal_id.clone(),
        stream_id,
        frames,
        frame_bytes,
        terminal_frame_bytes,
        ingress_active,
        ingress_cancel,
        Some(diagnostics.clone()),
    ));
    if let Err(failure) = bootstrap_attach_terminal(
        state,
        client_id,
        &terminal_id,
        core,
        &handle,
        &stream_tx,
        stream_id,
        selection.client_caps,
        selection.profile,
        selection.limits,
        token,
        None,
    )
    .await
    {
        plumbing.drop_stream_binding(&terminal_id).await;
        crate::runtime::send_error(&plumbing.out_tx, failure.code, &failure.message).await;
    } else {
        diagnostics.record_ready_latency(
            crate::stream_diagnostics::ReadyKind::Initial,
            bind_started.elapsed(),
        );
    }
}

/// Tear one Terminal stream down: unsubscribe, then drop the binding so its
/// writer drains and finishes the QUIC stream.
async fn teardown_terminal_stream(
    state: &SharedState,
    client_id: ClientId,
    plumbing: &mut ClientPlumbing,
    terminal_id: &WireResourceId,
) {
    handle_detach_terminal(state, client_id, terminal_id).await;
    plumbing.drop_stream_binding(terminal_id).await;
}

/// Write one legacy printable key on the receive task when the PTY writer
/// is idle. `true` means the event was consumed (written or refused).
fn try_inline_key(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &WireResourceId,
    input: &TerminalInput,
) -> bool {
    if !phux_perf::echo_hop::fast_path_enabled() {
        return false;
    }
    let TerminalInput::Key(event) = input else {
        return false;
    };
    let Some(prepared) = crate::runtime::with_attached_input_destination(
        state,
        client_id,
        terminal_id,
        "INPUT_KEY",
        |destination| {
            let options = destination.handle.input_snapshot.borrow().key;
            let text = crate::input::key::legacy_printable(event, options)?;
            let fast = destination.handle.fast_pty.as_ref()?.upgrade()?;
            Some((fast, text.as_bytes().to_vec()))
        },
    ) else {
        return false;
    };
    let Some((fast, bytes)) = prepared else {
        return false;
    };
    match fast.try_direct(&bytes) {
        crate::terminal_actor::DirectWrite::Wrote | crate::terminal_actor::DirectWrite::Handled => {
            true
        }
        crate::terminal_actor::DirectWrite::Busy => false,
    }
}

/// Route one decoded `INPUT_*` event: a local pane id goes to the input lane
/// (ADR-0044) when there is one, spending `credit` (ADR-0144); satellite ids
/// and the no-lane path use the inline [`handle_terminal_input`] with the
/// same gates.
pub(super) async fn route_client_input(
    state: &SharedState,
    input_lane: Option<&InputLaneHandle>,
    client_id: ClientId,
    terminal_id: WireResourceId,
    input: TerminalInput,
    credit: Option<crate::terminal_actor::InputCredit>,
) {
    phux_perf::echo_hop::record("server_recv");
    if try_inline_key(state, client_id, &terminal_id, &input) {
        return;
    }
    let frame_label = match &input {
        TerminalInput::Key(_) => "INPUT_KEY",
        TerminalInput::Mouse(_) => "INPUT_MOUSE",
        TerminalInput::Focus(_) => "INPUT_FOCUS",
        TerminalInput::Paste(_) => "INPUT_PASTE",
    };
    if let Some(lane) = input_lane
        && terminal_id.is_local()
    {
        // Arm before the send. `pending` is still zero while this key sits
        // in the lane thread; the next printable would otherwise write the
        // PTY ahead of it. `ROUTE_INPUT` and `APPLY_INPUT` arm in the lane
        // admission, so every source for the pane shares this count.
        let ahead = crate::runtime::input_lane::arm_pane_lane(state, &terminal_id);
        lane.route(
            RoutedInput::attached(client_id, terminal_id, input, frame_label)
                .with_credit(credit)
                .with_lane_ahead(ahead),
        )
        .await;
        return;
    }
    handle_terminal_input(state, client_id, &terminal_id, input, frame_label);
}

pub(crate) async fn abort_output_pumps(
    output_pumps: &mut JoinSet<()>,
    client_id: ClientId,
    reason: &'static str,
) {
    if output_pumps.is_empty() {
        return;
    }
    debug!(
        ?client_id,
        pump_count = output_pumps.len(),
        reason,
        "aborting per-attach output pumps",
    );
    output_pumps.abort_all();
    while output_pumps.join_next().await.is_some() {}
}
