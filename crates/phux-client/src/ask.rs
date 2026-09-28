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

/// Literal prefix of the ADR-0035 `phux-ask` terminal-title sentinel.
const ASK_TITLE_PREFIX: &str = "phux-ask";

/// A pending ask, as read out of a pane's terminal title: the client-side
/// reader of the ADR-0035 grammar the server's own `AskMarker` parses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskMarker {
    /// Stable question id an answer correlates against. Empty when the title
    /// omits the `[id]` segment — see [`AskMarker::is_identified`].
    pub id: String,
    /// The question text presented to the human.
    pub question: String,
    /// Suggested answers, in presentation order; empty when none were given.
    pub suggestions: Vec<String>,
}

impl AskMarker {
    /// Whether this ask carries an id an answer can be correlated against;
    /// an anonymous ask is indistinguishable from the next one worded alike.
    #[must_use]
    pub const fn is_identified(&self) -> bool {
        !self.id.is_empty()
    }

    /// The 1-based `index`'th suggestion, or `None` when out of range.
    #[must_use]
    pub fn suggestion(&self, index: usize) -> Option<&str> {
        index
            .checked_sub(1)
            .and_then(|zero_based| self.suggestions.get(zero_based))
            .map(String::as_str)
    }

    /// Whether `answer` is one of the published suggestions, compared trimmed
    /// and case-insensitively (a shell quoting artifact must not read as a
    /// different answer).
    #[must_use]
    pub fn lists(&self, answer: &str) -> bool {
        self.suggestions
            .iter()
            .any(|suggestion| suggestion.trim().eq_ignore_ascii_case(answer.trim()))
    }
}

/// Parse the ADR-0035 ask sentinel out of a pane's terminal title, or `None`
/// when the title is not one.
///
/// Grammar: `phux-ask`, an optional `[<id>]`, `:`, the question, then an
/// optional `?s=opt1|opt2` suggestion suffix. A bare `phux-ask` with no `:`
/// is not a marker (it carries no question).
#[must_use]
pub fn parse_ask_title(title: &str) -> Option<AskMarker> {
    let rest = title.strip_prefix(ASK_TITLE_PREFIX)?;
    let (id, rest) = if let Some(after_bracket) = rest.strip_prefix('[') {
        let close = after_bracket.find(']')?;
        (
            after_bracket[..close].to_owned(),
            &after_bracket[close + 1..],
        )
    } else {
        (String::new(), rest)
    };
    let body = rest.strip_prefix(':')?;
    let (question, suggestions) = match body.split_once("?s=") {
        Some((question, suggestions)) => (
            question.to_owned(),
            suggestions
                .split('|')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
        None => (body.to_owned(), Vec::new()),
    };
    Some(AskMarker {
        id,
        question,
        suggestions,
    })
}

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
    use super::{answer_events, parse_ask_title};
    use phux_protocol::input::InputEvent;
    use phux_protocol::input::paste::PasteTrust;

    /// The full sentinel: id, question, and the published suggestion set.
    #[test]
    fn a_full_sentinel_parses_into_id_question_and_suggestions() {
        let marker = parse_ask_title("phux-ask[deploy]:Deploy to prod??s=Yes|No|Hold")
            .expect("a full sentinel is a marker");
        assert_eq!(marker.id, "deploy");
        assert_eq!(marker.question, "Deploy to prod?");
        assert_eq!(marker.suggestions, ["Yes", "No", "Hold"]);
        assert!(marker.is_identified());
        assert_eq!(marker.suggestion(2), Some("No"));
        assert_eq!(marker.suggestion(4), None);
        // 1-based: index 0 must not silently mean the first suggestion.
        assert_eq!(marker.suggestion(0), None);
        assert!(marker.lists(" yes "));
        assert!(!marker.lists("maybe"));
    }

    /// An ask with no `[id]` parses, but is not answerable: the caller has to
    /// be able to tell "still that question" from "a new one worded the same".
    #[test]
    fn an_anonymous_sentinel_parses_but_is_not_identified() {
        let marker = parse_ask_title("phux-ask:Continue?").expect("an id is optional");
        assert_eq!(marker.id, "");
        assert_eq!(marker.question, "Continue?");
        assert!(marker.suggestions.is_empty());
        assert!(!marker.is_identified());
    }

    /// Everything that is not a sentinel reads as "no live ask" — including
    /// the degenerate prefix with no question after it.
    #[test]
    fn non_sentinel_titles_are_not_markers() {
        for title in [
            "",
            "vim README.md",
            "phux-ask",
            "phux-ask[q1]",
            "a phux-ask[q1]:Continue?",
            "phux-asked[q1]:Continue?",
        ] {
            assert!(
                parse_ask_title(title).is_none(),
                "'{title}' must not read as a live ask"
            );
        }
    }

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
