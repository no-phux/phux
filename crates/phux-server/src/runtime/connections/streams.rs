//! QUIC Terminal streams (docs/protocol.md#quic-stream-routing): binding a
//! client-opened stream to a resource it is subscribed to, bootstrapping it,
//! and tearing it down when it ends or fails.

use super::hello::NegotiatedConnection;
use super::plumbing::ClientPlumbing;
use crate::runtime::{
    bootstrap_attach_terminal, handle_detach_terminal, subscribe_attach_terminal,
};
use crate::state::{ClientId, SharedState};
use crate::transport::quic::{
    QuicStreamEvent, QuicStreamFailure, QuicWriter, pump_terminal_stream, refuse_terminal_stream,
};
use phux_dial::window::SendWindow;
use phux_protocol::ids::{ResourceId as WireResourceId, StreamId};
use phux_protocol::wire::frame::ErrorCode;
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// Handle one QUIC Terminal-stream event (docs/protocol.md#quic-stream-routing). `Bound` binds and
/// bootstraps the stream; `Ended` detaches as `DETACH_RESOURCE` would,
/// ignoring a stale generation.
pub(in crate::runtime) async fn handle_stream_event(
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
        s.subscribers_for_resource(local.id).contains(&client_id)
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
    let Some(profile) = selection.profile else {
        refuse_terminal_stream(send, recv);
        crate::runtime::send_error(
            &plumbing.out_tx,
            ErrorCode::CodecUnavailable,
            crate::runtime::apps::NO_TERMINAL_PROFILE,
        )
        .await;
        return;
    };
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
        Some(profile),
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
