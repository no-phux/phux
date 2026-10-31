//! `INPUT_*` frames: the Terminal's half of connection dispatch.

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{ErrorCode, FrameKind};

use super::terminal_streams::{direction_invalid, route_client_input};
use crate::runtime::connections::dispatch::{ConnectionEnding, FrameDispatch, FrameFlow};
use crate::runtime::connections::plumbing::dispatch_terminal_reply;
use crate::runtime::input_lane::InputStalled;
use crate::transport::FrameReader;
use phux_terminal::TerminalInput;

impl<R: FrameReader> FrameDispatch<'_, R> {
    /// `INPUT_*`: route the event (see [`is_input_frame`]).
    pub(in crate::runtime) async fn input(self, frame: FrameKind) -> FrameFlow {
        match frame {
            FrameKind::InputKey { terminal_id, event } => {
                return self
                    .route_input(terminal_id, TerminalInput::Key(event))
                    .await;
            }
            FrameKind::InputMouse { terminal_id, event } => {
                return self
                    .route_input(terminal_id, TerminalInput::Mouse(event))
                    .await;
            }
            FrameKind::InputFocus { terminal_id, event } => {
                return self
                    .route_input(terminal_id, TerminalInput::Focus(event))
                    .await;
            }
            FrameKind::InputPaste { terminal_id, event } => {
                return self
                    .route_input(terminal_id, TerminalInput::Paste(event))
                    .await;
            }
            FrameKind::InputTerminalReply { terminal_id, bytes } => {
                let Some(selection) = self.negotiated.as_ref() else {
                    return FrameFlow::Continue(());
                };
                dispatch_terminal_reply(
                    self.client_id,
                    *selection,
                    &terminal_id,
                    bytes,
                    &self.plumbing.out_tx,
                )
                .await;
            }
            other => return direction_invalid(self.client_id, &other),
        }
        FrameFlow::Continue(())
    }

    /// Route one structured input event. Lane-bound input first takes a
    /// pane input credit (ADR-0144), and while the pane is saturated this
    /// connection reads no further frame: the event waits rather than being
    /// dropped, and the transport pushes back on the sender. A pane that
    /// stays saturated past the stall limit refuses the event with an
    /// uncorrelated `ERROR(RESOURCE_EXHAUSTED)`.
    async fn route_input(self, terminal_id: WireResourceId, input: TerminalInput) -> FrameFlow {
        let lane = self.input_lane.filter(|_| terminal_id.is_local());
        let credit = match lane {
            Some(_) => {
                let acquired = tokio::select! {
                    biased;
                    () = self.token.cancelled() => {
                        return FrameFlow::Break(ConnectionEnding::Cancelled);
                    }
                    acquired = self.input_credits.acquire(self.state, &terminal_id) => acquired,
                };
                let Ok(credit) = acquired else {
                    crate::runtime::send_error(
                        &self.plumbing.out_tx,
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
        route_client_input(self.state, lane, self.client_id, terminal_id, input, credit).await;
        FrameFlow::Continue(())
    }
}
