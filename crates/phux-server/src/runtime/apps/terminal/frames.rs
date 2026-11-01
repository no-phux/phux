//! Client frames about a Terminal: input and its content stream.

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{ErrorCode, FrameKind};

use super::commands::{handle_frame_ack, handle_terminal_reply, handle_terminal_resize};
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
use super::history::{HistoryPageRequest, serve_history_request};
use super::input_lane::InputStalled;
use super::terminal_streams::route_client_input;
use crate::runtime::apps::ResourceFrame;
use crate::runtime::connections::dispatch::{ConnectionEnding, FrameFlow};
use crate::runtime::connections::frames::direction_invalid;
use phux_terminal::TerminalInput;

/// Serve one client frame about a Terminal.
pub(super) async fn serve(request: ResourceFrame<'_>, frame: FrameKind) -> FrameFlow {
    match frame {
        FrameKind::InputKey { terminal_id, event } => {
            return route_input(request, terminal_id, TerminalInput::Key(event)).await;
        }
        FrameKind::InputMouse { terminal_id, event } => {
            return route_input(request, terminal_id, TerminalInput::Mouse(event)).await;
        }
        FrameKind::InputFocus { terminal_id, event } => {
            return route_input(request, terminal_id, TerminalInput::Focus(event)).await;
        }
        FrameKind::InputPaste { terminal_id, event } => {
            return route_input(request, terminal_id, TerminalInput::Paste(event)).await;
        }
        FrameKind::InputTerminalReply { terminal_id, bytes } => {
            terminal_reply(&request, &terminal_id, &bytes).await;
        }
        FrameKind::ResizeTerminal {
            terminal_id,
            cols,
            rows,
            cell_px,
        } => handle_terminal_resize(
            request.state,
            request.client_id,
            &terminal_id,
            (cols, rows),
            cell_px,
        ),
        FrameKind::FrameAck {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
        } => handle_frame_ack(
            request.state,
            request.client_id,
            &terminal_id,
            stream_id,
            bootstrap_id,
            seq,
        ),
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        FrameKind::HistoryRequest {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            max_bytes,
            max_rows,
        } => {
            let Some(selection) = request.selection else {
                return FrameFlow::Continue(());
            };
            // Terminal-content replies ride the bound stream when one
            // exists (docs/protocol.md#quic-stream-routing); control otherwise.
            let history_tx = request.plumbing.sender_for(&terminal_id);
            serve_history_request(
                request.state,
                request.client_id,
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
        other => return direction_invalid(request.client_id, &other),
    }
    FrameFlow::Continue(())
}

/// `INPUT_TERMINAL_REPLY`: refused unless this connection negotiated it.
async fn terminal_reply(request: &ResourceFrame<'_>, terminal_id: &WireResourceId, bytes: &[u8]) {
    let Some(selection) = request.selection else {
        return;
    };
    if selection.accepts_terminal_reply() {
        handle_terminal_reply(request.client_id, terminal_id, bytes);
        return;
    }
    crate::runtime::send_error(
        &request.plumbing.out_tx,
        ErrorCode::UnknownMessageType,
        "INPUT_TERMINAL_REPLY was not advertised for this connection",
    )
    .await;
}

/// Route one structured input event. Lane-bound input first takes a
/// pane input credit (ADR-0144), and while the pane is saturated this
/// connection reads no further frame: the event waits rather than being
/// dropped, and the transport pushes back on the sender. A pane that
/// stays saturated past the stall limit refuses the event with an
/// uncorrelated `ERROR(RESOURCE_EXHAUSTED)`.
async fn route_input(
    request: ResourceFrame<'_>,
    terminal_id: WireResourceId,
    input: TerminalInput,
) -> FrameFlow {
    let lane = request.input_lane.filter(|_| terminal_id.is_local());
    let credit = match lane {
        Some(_) => {
            let acquired = tokio::select! {
                biased;
                () = request.token.cancelled() => {
                    return FrameFlow::Break(ConnectionEnding::Cancelled);
                }
                acquired = request.input_credits.acquire(request.state, &terminal_id) => acquired,
            };
            let Ok(credit) = acquired else {
                crate::runtime::send_error(
                    &request.plumbing.out_tx,
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
    route_client_input(
        request.state,
        lane,
        request.client_id,
        terminal_id,
        input,
        credit,
    )
    .await;
    FrameFlow::Continue(())
}
