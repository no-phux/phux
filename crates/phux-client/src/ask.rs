//! The agent-ask round trip: report a pending question, and answer one.
//!
//! Two directions, both over the existing wire:
//!
//! - **Reporting** ([`report`]) is the opt-in hook ingress of
//!   [ADR-0036](../../../ADR/0036-agent-asked-detection.md) point 1. An
//!   integration whose agent has blocked for a human answer calls it, and the
//!   server emits the normal `AgentEvent::Asked` without the agent having to
//!   write an OSC title sentinel into its own pane.
//! - **Answering** ([`parse_ask_title`] + [`deliver_answer`]) is the return
//!   leg. It exists because `Asked` carries the *suggestions the asking agent
//!   itself published* ([ADR-0035](../../../ADR/0035-agent-asked-event.md)),
//!   so an orchestrator can reply with a validated choice instead of a blind
//!   keystroke.
//!
//! # Why answering reads the title rather than asking the server
//!
//! The server owns the live ask (`AskedDetector`), but nothing on the wire
//! reads it back: `Asked` is broadcast on the event stream and there is no
//! query command. The ADR-0035 sentinel, though, lives in the pane's *title*,
//! and `GET_STATE` already carries every pane's title — so for a
//! sentinel-sourced ask the live question is readable client-side, with no new
//! wire surface and no server round trip the client does not already make
//! (ADR-0021: selectors, and now ask liveness, resolve client-side).
//!
//! That is a real limit, stated rather than papered over: an ask reported
//! through [`report`] sets no title, so [`parse_ask_title`] cannot see it. A
//! hook-sourced ask is answerable only once the server grows a read-back for
//! its own detector state.

use std::path::Path;

use phux_protocol::TerminalId;
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

/// Literal prefix of the ADR-0035 `phux-ask` terminal-title sentinel.
const ASK_TITLE_PREFIX: &str = "phux-ask";

/// A pending ask, as read out of a pane's terminal title.
///
/// The client-side mirror of the server's own `AskMarker`
/// (`crates/phux-server/src/terminal_actor/mod.rs`), parsing the identical
/// grammar. The duplication is deliberate and load-bearing: the server parses
/// the title to *emit* the event, and a client parses the same title to check
/// the ask it was told about is still the live one. Two readers of one
/// documented wire-adjacent format, not two sources of truth — the grammar is
/// normative in ADR-0035 and neither copy may extend it unilaterally.
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
    /// Whether this ask carries an id an answer can be correlated against.
    ///
    /// An anonymous ask (`phux-ask:Continue?`) is indistinguishable from the
    /// *next* anonymous ask with the same text, so it cannot be answered
    /// safely: the "is this still the question I read?" check would pass
    /// across a question boundary. Callers refuse rather than guess.
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
/// the real Enter key.
///
/// Deliberately built here rather than by routing `[text, "Enter"]` through
/// [`crate::send_keys::events_for`]. That function interprets its arguments as
/// key *specs*, and a perfectly ordinary answer — `up`, `esc`, `space`, `C-c`
/// — is a named spec. Answering an agent's "which direction?" with `up` would
/// send an arrow key instead of the word. An answer is never a key spec; it is
/// text the asking agent published, and it is typed verbatim.
///
/// One batch with Enter last is the ADR-0076 shape: a partial write can only
/// truncate the tail, and the tail is the submission, so the worst case is an
/// unsubmitted answer sitting visible in the composer rather than a truncated
/// one submitted with confidence.
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

/// Deliver `text` as the answer to `pane`'s pending ask, over `conn`.
///
/// Delegates the write itself to [`crate::agent_prompt::apply_input_once`] —
/// the same acknowledged path `agent prompt` uses — rather than issuing its own
/// `APPLY_INPUT`. That is deliberate: the interesting part of an acknowledged
/// write is not sending it, it is *reading the reply*, and
/// [`crate::agent_prompt::classify`] already encodes the one reading each
/// `ErrorCode` may be given, including the rule that a code this build does not
/// recognise pessimizes to [`ApplyVerdict::Unknown`]. A second copy of that
/// table is a second place for an optimistic default to creep in, and the
/// optimistic default here is a duplicated submission.
///
/// `operation_id` must come from a CSPRNG. It is the key to the server's
/// 10-minute dedupe horizon, under which re-sending the *same* id and payload
/// replays the cached result instead of typing twice; a guessable id lets an
/// unrelated client bind that entry first and turn a legitimate answer into an
/// id-reuse conflict.
///
/// Takes an existing connection rather than a socket path so the caller can
/// read the pane's title and write the answer as consecutive frames on one
/// connection: the server handles a connection's frames in arrival order, so
/// nothing this client sends can interleave between the liveness check and the
/// write.
///
/// # Errors
///
/// Returns [`AttachError`] on transport failure. A server *refusal* is not an
/// error here — it is an [`ApplyVerdict`], because "the batch was refused" and
/// "the connection broke" are different questions with different remedies.
pub async fn deliver_answer(
    conn: &mut Connection,
    pane: &TerminalId,
    operation_id: InputOperationId,
    request_id: u32,
    text: &str,
) -> Result<ApplyVerdict, AttachError> {
    let (verdict, interleaved) =
        apply_input_once(conn, pane, operation_id, answer_events(text), request_id).await?;
    // This connection subscribed to nothing and the input lane emits no frame
    // of its own ahead of the ack, so anything here would be a server bug
    // rather than a transition worth keeping (same argument as `report`).
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
        // 1-based: index 0 is not a choice, and must not silently mean the
        // first suggestion.
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

    /// The answer is typed verbatim, never re-interpreted as a key spec. `up`
    /// is a word here, not an arrow key — this is the regression that would
    /// otherwise send `ESC [ A` into someone's turn.
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
