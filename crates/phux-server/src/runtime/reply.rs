//! The one way a deferred or routed command answers its requester.

use phux_protocol::wire::frame::{CommandResult, FrameKind};

use crate::state::Outbound;

/// Queue `result` as the `COMMAND_RESULT` for `request_id`; a closed mailbox
/// means the connection is already gone.
pub(super) async fn send(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    request_id: u32,
    result: CommandResult,
) {
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::CommandResult {
            request_id,
            result,
        }))
        .await;
}
