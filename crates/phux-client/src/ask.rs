//! The agent-ask round trip: [`report`] a pending question (the ADR-0036
//! hook ingress), and answer one ([`parse_ask_title`] + [`deliver_answer`])
//! with a choice the asking agent published (ADR-0035).
//!
//! Answering reads the live ask from the pane's title sentinel, which
//! `GET_STATE` already carries; nothing on the wire reads the server's own
//! ask state back. So a hook-reported ask (which sets no title) cannot be
//! answered this way.

use std::path::Path;

use phux_protocol::ResourceId;
use phux_protocol::ids::InputOperationId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{Command, CommandResult};

use crate::agent_prompt::{ApplyVerdict, apply_input_once};
use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::attach::input::StdinParser;

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
    pane: ResourceId,
    payload: AskedPayload,
) -> Result<(), AttachError> {
    let mut conn = Connection::connect(socket).await?;
    // This fresh connection never subscribes, so nothing interleaves.
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

pub use phux_client_core::ask::{
    AskMarker, MAX_ANSWER_BYTES, answer_text_refusal, parse_ask_title,
};

/// The input batch that answers an ask with `text`: one trusted paste, then
/// Enter.
///
/// Not [`crate::send_keys::events_for`], which would read an answer like `up`
/// as an arrow key. Enter last means a partial write can only lose
/// the submission (ADR-0076).
#[must_use]
pub fn answer_events(text: &str) -> Vec<InputEvent> {
    let mut parser = StdinParser::default();
    let mut events = vec![InputEvent::Paste(PasteEvent {
        trust: PasteTrust::Trusted,
        data: text.as_bytes().to_vec(),
    })];
    events.extend(parser.feed(b"\r"));
    events.extend(parser.flush());
    events
}

/// Deliver `text` as the answer to `pane`'s pending ask, over `conn`, with
/// one acknowledged [`apply_input_once`] (the verdict table `agent prompt`
/// uses).
///
/// `operation_id` must come from a CSPRNG: it keys the server's dedupe
/// horizon. The caller reads the title and writes the answer on the same
/// connection, so nothing can interleave between check and write. A
/// [`ApplyVerdict::Busy`] is surfaced, not retried: the liveness check would
/// go stale during a backoff.
///
/// # Errors
///
/// Transport failure. A server refusal is an [`ApplyVerdict`], not an error.
pub async fn deliver_answer(
    conn: &mut Connection,
    pane: &ResourceId,
    operation_id: InputOperationId,
    request_id: u32,
    text: &str,
) -> Result<ApplyVerdict, AttachError> {
    let (verdict, interleaved) =
        apply_input_once(conn, pane, operation_id, answer_events(text), request_id).await?;
    // Unsubscribed: anything interleaved would be a server bug.
    debug_assert!(
        interleaved.is_empty(),
        "unsubscribed connection received {interleaved:?} ahead of the APPLY_INPUT ack",
    );
    Ok(verdict)
}

#[cfg(test)]
mod tests {
    use super::answer_events;
    use phux_protocol::input::InputEvent;
    use phux_protocol::input::paste::PasteTrust;

    /// The answer is typed verbatim, never re-interpreted as a key spec: `up`
    /// is a word here, not `ESC [ A`.
    #[test]
    fn an_answer_that_looks_like_a_key_spec_is_still_typed_as_text() {
        for answer in ["up", "esc", "space", "C-c", "yes"] {
            let events = answer_events(answer);
            assert_eq!(events.len(), 2, "{answer}: paste + Enter");
            match &events[0] {
                InputEvent::Paste(paste) => {
                    assert_eq!(paste.trust, PasteTrust::Trusted);
                    assert_eq!(paste.data, answer.as_bytes());
                }
                other => panic!("{answer}: expected a paste first, got {other:?}"),
            }
            assert!(
                matches!(events[1], InputEvent::Key(_)),
                "{answer}: the submission must be a real key event"
            );
        }
    }
}
