//! Commands sharing the Terminal input lane and a connection's credit state.

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{Command, CommandResult};

use super::{InputCredits, InputLaneHandle, InputReceipt, InputStalled};
use crate::state::{ClientId, SharedState};

impl InputLaneHandle {
    /// Acknowledged and routed input to a local Terminal rides the lane.
    pub(crate) const fn rides(command: &Command) -> bool {
        matches!(
            command,
            Command::ApplyInput { terminal_id, .. } | Command::RouteInput { terminal_id, .. }
                if matches!(terminal_id, WireResourceId::Local { .. })
        )
    }

    /// Open a connection's credit state on the shared input lane.
    pub(crate) fn connect(&self) -> ConnectionLane {
        ConnectionLane {
            lane: self.clone(),
            credits: InputCredits::default(),
        }
    }
}

/// The common FIFO and the credit state of one connection's read loop.
#[derive(Debug)]
pub(crate) struct ConnectionLane {
    lane: InputLaneHandle,
    credits: InputCredits,
}

impl ConnectionLane {
    pub(crate) const fn lane(&self) -> &InputLaneHandle {
        &self.lane
    }

    pub(crate) const fn parts(&mut self) -> (&InputLaneHandle, &mut InputCredits) {
        (&self.lane, &mut self.credits)
    }

    /// Begin a command already selected by [`InputLaneHandle::rides`].
    pub(crate) async fn begin(
        &mut self,
        state: &SharedState,
        client_id: ClientId,
        command: Command,
    ) -> Result<InputReceipt, CommandResult> {
        match command {
            Command::ApplyInput {
                operation_id,
                terminal_id,
                events,
            } => Ok(self
                .lane
                .begin_apply(state, client_id, operation_id, terminal_id, events)),
            Command::RouteInput { terminal_id, event } => {
                // Same FIFO and the same wait as `INPUT_*` (ADR-0144).
                let credit = self
                    .credits
                    .acquire(state, &terminal_id)
                    .await
                    .map_err(|_| InputStalled::result(&terminal_id))?;
                Ok(self
                    .lane
                    .begin_route(state, client_id, terminal_id, event, credit))
            }
            _ => unreachable!("guarded input command"),
        }
    }
}
