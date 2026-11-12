//! The Terminal's stream and input frames: its half of connection dispatch
//! ([`ResourceApp::frame`](crate::runtime::apps::ResourceApp::frame)).

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{ErrorCode, FrameKind};

use super::commands::{
    handle_frame_ack, handle_terminal_input, handle_terminal_reply, handle_terminal_resize,
};
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
use super::history::{HistoryPageRequest, serve_history_request};
use super::input_lane::{InputLaneHandle, InputStalled, RoutedInput};
use crate::runtime::apps::ResourceFrame;
use crate::runtime::connections::dispatch::{ConnectionEnding, FrameFlow, direction_invalid};
use crate::runtime::connections::hello::NegotiatedConnection;
use crate::state::{ClientId, Outbound, SharedState};
use phux_terminal_api::TerminalInput;
use tokio::sync::mpsc;

/// Serve one `INPUT_*`, `RESIZE_TERMINAL`, `FRAME_ACK` or `HISTORY_REQUEST`
/// frame about a Terminal.
pub(super) async fn serve_frame(ctx: ResourceFrame<'_>, frame: FrameKind) -> FrameFlow {
    match frame {
        FrameKind::InputKey { terminal_id, event } => {
            return route_input(ctx, terminal_id, TerminalInput::Key(event)).await;
        }
        FrameKind::InputMouse { terminal_id, event } => {
            return route_input(ctx, terminal_id, TerminalInput::Mouse(event)).await;
        }
        FrameKind::InputFocus { terminal_id, event } => {
            return route_input(ctx, terminal_id, TerminalInput::Focus(event)).await;
        }
        FrameKind::InputPaste { terminal_id, event } => {
            return route_input(ctx, terminal_id, TerminalInput::Paste(event)).await;
        }
        FrameKind::ClipboardReply {
            terminal_id,
            selection,
            text,
        } => {
            let Some(input) = super::clipboard_read::admit(&ctx, &terminal_id, selection, text)
            else {
                return FrameFlow::Continue(());
            };
            return route_input(ctx, terminal_id, input).await;
        }
        FrameKind::InputTerminalReply { terminal_id, bytes } => {
            let Some(selection) = ctx.negotiated else {
                return FrameFlow::Continue(());
            };
            dispatch_terminal_reply(
                ctx.client_id,
                selection,
                &terminal_id,
                bytes,
                &ctx.plumbing.out_tx,
            )
            .await;
        }
        FrameKind::ResizeTerminal {
            terminal_id,
            cols,
            rows,
            cell_px,
        } => {
            handle_terminal_resize(
                ctx.state,
                ctx.client_id,
                &terminal_id,
                (cols, rows),
                cell_px,
            );
        }
        FrameKind::FrameAck {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
        } => {
            handle_frame_ack(
                ctx.state,
                ctx.client_id,
                &terminal_id,
                stream_id,
                bootstrap_id,
                seq,
            );
        }
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        FrameKind::HistoryRequest {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            max_bytes,
            max_rows,
        } => {
            let Some(selection) = ctx.negotiated else {
                return FrameFlow::Continue(());
            };
            // Terminal-content replies ride the bound stream when one
            // exists (docs/protocol.md#quic-stream-routing); control otherwise.
            let history_tx = ctx.plumbing.sender_for(&terminal_id);
            serve_history_request(
                ctx.state,
                ctx.client_id,
                &history_tx,
                selection,
                HistoryPageRequest {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    cursor,
                    max_bytes,
                    max_rows,
                },
            )
            .await;
        }
        other => return direction_invalid(ctx.client_id, &other),
    }
    FrameFlow::Continue(())
}

/// Route one structured input event. Lane-bound input first takes a
/// pane input credit (ADR-0144), and while the pane is saturated this
/// connection reads no further frame: the event waits rather than being
/// dropped, and the transport pushes back on the sender. A pane that
/// stays saturated past the stall limit refuses the event with an
/// uncorrelated `ERROR(RESOURCE_EXHAUSTED)`.
async fn route_input(
    ctx: ResourceFrame<'_>,
    terminal_id: WireResourceId,
    input: TerminalInput,
) -> FrameFlow {
    let parts = ctx
        .lane
        .and_then(super::input_lane::parts_of)
        .filter(|_| terminal_id.is_local());
    let lane = parts.as_ref().map(|(lane, _)| *lane);
    let credit = match parts {
        Some((_, credits)) => {
            let acquired = tokio::select! {
                biased;
                () = ctx.token.cancelled() => {
                    return FrameFlow::Break(ConnectionEnding::Cancelled);
                }
                acquired = credits.acquire(ctx.state, &terminal_id) => acquired,
            };
            let Ok(credit) = acquired else {
                crate::runtime::send_error(
                    &ctx.plumbing.out_tx,
                    ErrorCode::ResourceExhausted,
                    &InputStalled::message(&terminal_id),
                )
                .await;
                return FrameFlow::Continue(());
            };
            credit
        }
        None => None,
    };
    // The host accounted for view activity before delivery admission. Do not
    // count accepted input a second time after waiting for its lane credit.
    route_client_input(ctx.state, lane, ctx.client_id, terminal_id, input, credit).await;
    FrameFlow::Continue(())
}

/// Hand one `INPUT_TERMINAL_REPLY` to the pane if the connection advertised
/// it; otherwise report the type without ending the connection.
async fn dispatch_terminal_reply(
    client_id: ClientId,
    selection: NegotiatedConnection,
    terminal_id: &WireResourceId,
    bytes: bytes::Bytes,
    out_tx: &mpsc::Sender<Outbound>,
) {
    if !selection.accepts_terminal_reply() {
        crate::runtime::send_error(
            out_tx,
            ErrorCode::UnknownMessageType,
            "INPUT_TERMINAL_REPLY was not advertised for this connection",
        )
        .await;
        return;
    }
    handle_terminal_reply(client_id, terminal_id, &bytes);
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
    if !matches!(input, TerminalInput::Key(_)) {
        return false;
    }
    let result = super::commands::with_attached_input_destination(
        state,
        client_id,
        terminal_id,
        "INPUT_KEY",
        |destination| destination.handle.ordered_input.try_inline(input),
    );
    matches!(
        result,
        Some(
            phux_terminal_api::input::DirectWrite::Wrote
                | phux_terminal_api::input::DirectWrite::Handled
        )
    )
}

/// Route one decoded `INPUT_*` event: a local pane id goes to the input lane
/// (ADR-0044) when there is one, spending `credit` (ADR-0144); satellite ids
/// and the no-lane path use the inline [`handle_terminal_input`] with the
/// same gates.
async fn route_client_input(
    state: &SharedState,
    input_lane: Option<&InputLaneHandle>,
    client_id: ClientId,
    terminal_id: WireResourceId,
    input: TerminalInput,
    credit: Option<phux_terminal_api::actor::InputCredit>,
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
        TerminalInput::ClipboardReply { .. } => "CLIPBOARD_REPLY",
    };
    if let Some(lane) = input_lane
        && terminal_id.is_local()
    {
        // Arm before the send. `pending` is still zero while this key sits
        // in the lane thread; the next printable would otherwise write the
        // PTY ahead of it. `ROUTE_INPUT` and `APPLY_INPUT` arm in the lane
        // admission, so every source for the pane shares this count.
        let ahead = super::input_lane::arm_pane_lane(state, &terminal_id);
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
