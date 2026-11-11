//! The lane as the host carries it ([`Lane`]): which commands ride it, each
//! connection's input credits, and starting it for the server.

use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode};

use super::{InputCredits, InputLaneHandle, InputStalled, spawn_input_lane};
use crate::runtime::apps::lane::{ConnectionLane, InputLane, Lane, LaneCommand, RunningLane};
use crate::state::SharedState;

impl Lane for InputLaneHandle {
    /// Acknowledged and routed input to a local Terminal.
    fn rides(&self, command: &Command) -> bool {
        matches!(
            command,
            Command::ApplyInput { terminal_id, .. } | Command::RouteInput { terminal_id, .. }
                if matches!(terminal_id, WireResourceId::Local { .. })
        )
    }

    fn connect(&self) -> Box<dyn std::any::Any> {
        Box::new(InputCredits::default())
    }

    fn begin<'a>(
        &'a self,
        on: LaneCommand<'a>,
        command: Command,
    ) -> LocalBoxFuture<'a, Result<LocalBoxFuture<'static, CommandResult>, CommandResult>> {
        async move {
            let receipt = match command {
                Command::ApplyInput {
                    operation_id,
                    terminal_id,
                    events,
                } => self.begin_apply(on.state, on.client_id, operation_id, terminal_id, events),
                Command::RouteInput { terminal_id, event } => {
                    let Some(credits) = on.connection.downcast_mut::<InputCredits>() else {
                        return Err(CommandResult::Error {
                            code: ErrorCode::InternalError,
                            message: "input lane connection state unavailable".to_owned(),
                        });
                    };
                    // Same FIFO and the same wait as `INPUT_*` (ADR-0144).
                    let Ok(credit) = credits.acquire(on.state, &terminal_id).await else {
                        return Err(InputStalled::result(&terminal_id));
                    };
                    self.begin_route(on.state, on.client_id, terminal_id, event, credit)
                }
                _ => unreachable!("guarded input command"),
            };
            Ok(receipt.boxed_local())
        }
        .boxed_local()
    }
}

/// The server's input lane, on its own OS thread.
pub(crate) fn start(state: &SharedState) -> std::io::Result<RunningLane> {
    let lane = spawn_input_lane(state.clone())?;
    Ok(RunningLane::new(InputLane::new(lane.handle()), lane))
}

/// This app's handle on `lane`.
pub(crate) fn handle_of(lane: &InputLane) -> Option<&InputLaneHandle> {
    lane.get()
}

/// A connection's lane handle and input credits.
pub(crate) fn parts_of(lane: &mut ConnectionLane) -> Option<(&InputLaneHandle, &mut InputCredits)> {
    lane.parts()
}
