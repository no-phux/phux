//! The Terminal's input and stream frames: its half of connection dispatch.

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{ErrorCode, FrameKind};

use super::terminal_streams::{direction_invalid, is_input_frame, route_client_input};
use crate::runtime::apps::StreamFrameContext;
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
use crate::runtime::apps::terminal::history::{HistoryPageRequest, serve_history_request};
use crate::runtime::connections::dispatch::{ConnectionEnding, FrameFlow};
use crate::runtime::connections::plumbing::dispatch_terminal_reply;
use crate::runtime::input_lane::InputStalled;
use crate::runtime::{handle_frame_ack, handle_terminal_resize};
use phux_terminal::TerminalInput;

/// Serve one frame addressed to a Terminal (see
/// [`crate::runtime::apps::addressed_resource`]).
pub(super) async fn serve(ctx: StreamFrameContext<'_>, frame: FrameKind) -> FrameFlow {
    if is_input_frame(&frame) {
        input(ctx, frame).await
    } else {
        stream(ctx, frame).await
    }
}

/// `INPUT_*`: route the event (see [`is_input_frame`]).
async fn input(ctx: StreamFrameContext<'_>, frame: FrameKind) -> FrameFlow {
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
    ctx: StreamFrameContext<'_>,
    terminal_id: WireResourceId,
    input: TerminalInput,
) -> FrameFlow {
    let lane = ctx.input_lane.filter(|_| terminal_id.is_local());
    let credit = match lane {
        Some(_) => {
            let acquired = tokio::select! {
                biased;
                () = ctx.token.cancelled() => {
                    return FrameFlow::Break(ConnectionEnding::Cancelled);
                }
                acquired = ctx.input_credits.acquire(ctx.state, &terminal_id) => acquired,
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
    route_client_input(ctx.state, lane, ctx.client_id, terminal_id, input, credit).await;
    FrameFlow::Continue(())
}

/// Frames about one attached Terminal's size or content stream.
async fn stream(ctx: StreamFrameContext<'_>, frame: FrameKind) -> FrameFlow {
    match frame {
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
