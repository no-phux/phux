//! Explicit agent-ask hook client.
//!
//! Integrations use this module when an agent has entered a blocked
//! human-answerable state and wants phux to emit the normal `asked` event
//! without writing an OSC title sentinel into the pane.

use std::path::Path;

use phux_protocol::TerminalId;
use phux_protocol::wire::frame::{Command, CommandResult};

use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// Payload reported by an opt-in agent ask hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskedPayload {
    /// Stable question id for answer correlation.
    pub id: String,
    /// Human-facing question text.
    pub question: String,
    /// Suggested answers, in display order.
    pub suggestions: Vec<String>,
    /// Optional seconds the agent has already been waiting.
    pub elapsed_seconds: Option<u64>,
}

/// Report an agent ask for `pane` and wait for the server acknowledgement.
///
/// The server validates the payload and broadcasts `AgentEvent::Asked` to the
/// existing event stream. This function does not attach, resize, or write to
/// the target PTY.
///
/// # Errors
///
/// Returns [`AttachError`] on connect/transport/protocol failure, unknown
/// target pane, or server-side payload rejection.
pub async fn report(
    socket: &Path,
    pane: TerminalId,
    payload: AskedPayload,
) -> Result<(), AttachError> {
    let mut conn = Connection::connect(socket).await?;
    // `handle_report_asked` broadcasts the resulting `Asked` event to the
    // pane's *event subscribers* before returning its ack — so on a
    // connection that had subscribed, the event would arrive ahead of the
    // COMMAND_RESULT. A hook reporter never subscribes: this connection is
    // opened here, sends exactly one command, and is dropped. Nothing can fan
    // out onto its mailbox, so there is nothing to keep.
    match conn
        .request(
            1,
            Command::ReportAsked {
                terminal_id: pane,
                id: payload.id,
                question: payload.question,
                suggestions: payload.suggestions,
                elapsed_seconds: payload.elapsed_seconds,
            },
        )
        .await?
        .into_result_ignoring_interleaved()
    {
        CommandResult::Ok => Ok(()),
        CommandResult::Error { message, .. } => Err(AttachError::Refused(message)),
        other => Err(AttachError::Protocol(crate::explain::explain_unexpected(
            "REPORT_ASKED",
            &other,
        ))),
    }
}
