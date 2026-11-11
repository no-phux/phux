//! `phux agent answer` — reply to a pending agent question by **validated
//! choice**, not by blind keystroke.
//!
//! The bytes typed are always a string the asking agent published in its
//! ADR-0035 ask (`--choice N` sends `suggestions[N-1]` verbatim, `--text T`
//! is checked against the same list), unless `--allow-unlisted` is passed.
//!
//! Refusals: no live ask (`no_active_ask`); a different question than the
//! `--id` named (`ask_stale`, the failure this verb exists to prevent); and an
//! ask with no id (`ask_unidentified`), which cannot be told apart from the
//! next one worded the same way.
//!
//! Delivery is one acknowledged `APPLY_INPUT` batch of trusted paste + Enter
//! under a fresh CSPRNG operation id (ADR-0053, ADR-0076). Double submission is
//! prevented by the staleness gate: a delivered answer retitles the pane, so a
//! second answer finds no live ask. Ids are never derived from content, since
//! a derivable id is a guessable one.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::agent_prompt::{ApplyVerdict, supports_acknowledged_input};
use phux_client::ask::{AskMarker, MAX_ANSWER_BYTES, deliver_answer, parse_ask_title};
use phux_client::attach::connection::Connection;
use phux_client::read::Verb;
use phux_protocol::ids::InputOperationId;
use phux_server::runtime::default_socket_path;

use crate::commands::json_err::{Refusal, codes};
use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target_for_input};

/// Confirm the pane is still asking the question `expected_id` names.
fn live_ask(title: Option<&str>, expected_id: &str) -> Result<AskMarker, Refusal> {
    let Some(marker) = title.and_then(parse_ask_title) else {
        return Err(Refusal::new(
            codes::NO_ACTIVE_ASK,
            "the pane is not asking anything (its title carries no phux-ask sentinel)",
            "nothing was typed. `phux agent show` reads the pane's current state, and \
             `phux watch --json` reports the `asked` event when a question appears. If \
             you meant to type into the pane regardless, that is `phux send-keys`",
            crate::exit_codes::EXIT_USAGE,
        ));
    };
    if !marker.is_identified() {
        return Err(Refusal::new(
            codes::ASK_UNIDENTIFIED,
            format!(
                "the pane is asking '{}' but the ask carries no id, so an answer cannot \
                 be correlated to it",
                marker.question
            ),
            "nothing was typed. An anonymous ask cannot be told apart from the next one \
             worded the same way, which is exactly the confusion --id exists to prevent. \
             Have the agent set `phux-ask[<id>]:<question>` (or report it with \
             `phux ask --id`)",
            crate::exit_codes::EXIT_USAGE,
        ));
    }
    if marker.id != expected_id {
        return Err(Refusal::new(
            codes::ASK_STALE,
            format!(
                "the pane has moved on: it is now asking '{}' (id '{}'), not the question \
                 you named (id '{expected_id}')",
                marker.question, marker.id
            ),
            "nothing was typed — answering a question the agent already passed would put \
             your answer into whatever is on screen now. Re-read the live ask and answer \
             that one",
            crate::exit_codes::EXIT_USAGE,
        ));
    }
    Ok(marker)
}

/// Which flag produced the answer, for the success document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnswerSource {
    /// `--choice N`, resolved through the live suggestion list.
    Choice,
    /// `--text T`, matched against the live suggestion list.
    Text,
    /// `--text T --allow-unlisted`, deliberately outside the list.
    Unlisted,
}

impl AnswerSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Choice => "choice",
            Self::Text => "text",
            Self::Unlisted => "unlisted",
        }
    }
}

/// Resolve the caller's flags against the **live** marker into the exact
/// string to type.
fn select_answer(
    marker: &AskMarker,
    choice: Option<usize>,
    text: Option<&str>,
    allow_unlisted: bool,
) -> Result<(String, AnswerSource), Refusal> {
    if let Some(index) = choice {
        if marker.suggestions.is_empty() {
            return Err(Refusal::new(
                codes::NO_SUGGESTIONS,
                format!(
                    "the ask '{}' published no suggestions, so there is no choice {index} \
                     to make",
                    marker.question
                ),
                "nothing was typed. Answer it with --text, which types exactly what you \
                 pass",
                crate::exit_codes::EXIT_USAGE,
            ));
        }
        let Some(suggestion) = marker.suggestion(index) else {
            return Err(Refusal::new(
                codes::CHOICE_OUT_OF_RANGE,
                format!(
                    "choice {index} is outside the ask's {} suggestion(s): {}",
                    marker.suggestions.len(),
                    numbered(&marker.suggestions)
                ),
                "nothing was typed. Choices are 1-based and resolve against the ask that \
                 is live right now",
                crate::exit_codes::EXIT_USAGE,
            ));
        };
        return validate_answer(suggestion).map(|answer| (answer, AnswerSource::Choice));
    }

    let Some(text) = text else {
        return Err(no_answer_refusal());
    };
    if marker.suggestions.is_empty() || marker.lists(text) {
        // An open ask (no published set) has nothing to validate against, so
        // free-form text is the only thing it *can* take.
        return validate_answer(text).map(|answer| (answer, AnswerSource::Text));
    }
    if allow_unlisted {
        return validate_answer(text).map(|answer| (answer, AnswerSource::Unlisted));
    }
    Err(Refusal::new(
        codes::UNLISTED_ANSWER,
        format!(
            "'{text}' is not one of the answers the agent offered: {}",
            numbered(&marker.suggestions)
        ),
        "nothing was typed. Answering with a string the agent published is what makes \
         this verb safer than send-keys. Use --choice N to pick one, or --allow-unlisted \
         if you have read the question and know free text is accepted",
        crate::exit_codes::EXIT_USAGE,
    ))
}

/// Neither `--choice` nor `--text`.
fn no_answer_refusal() -> Refusal {
    Refusal::new(
        codes::NO_ANSWER,
        "no answer given",
        "pass --choice N to send one of the ask's published suggestions, or --text T to \
         send exactly T",
        crate::exit_codes::EXIT_USAGE,
    )
}

/// Reject an answer that cannot be typed safely. A line break becomes an
/// extra submission on a pane without bracketed paste, which no client can
/// observe, so it is refused outright.
fn validate_answer(answer: &str) -> Result<String, Refusal> {
    if answer.trim().is_empty() {
        return Err(Refusal::new(
            codes::INVALID_ANSWER,
            "the answer is empty",
            "nothing was typed. An empty answer submits a bare Enter, which is a \
             different act — send it with `phux send-keys TARGET Enter` if that is what \
             you mean",
            crate::exit_codes::EXIT_USAGE,
        ));
    }
    if answer.contains('\n') || answer.contains('\r') {
        return Err(Refusal::new(
            codes::INVALID_ANSWER,
            "the answer contains a line break",
            "nothing was typed. On a pane that has not enabled bracketed paste, each \
             line break becomes its own submission, and phux cannot observe that mode \
             from here. Answer on one line, or use `phux agent prompt` for multi-line \
             text",
            crate::exit_codes::EXIT_USAGE,
        ));
    }
    if answer.len() > MAX_ANSWER_BYTES {
        return Err(Refusal::new(
            codes::INVALID_ANSWER,
            format!(
                "the answer is {} bytes; the limit is {MAX_ANSWER_BYTES}",
                answer.len()
            ),
            "nothing was typed. An answer this long is prose — `phux agent prompt` is \
             the verb for prose",
            crate::exit_codes::EXIT_USAGE,
        ));
    }
    Ok(answer.to_owned())
}

/// Render a suggestion list as `1) a  2) b`, for a diagnostic that has to tell
/// the caller what the valid choices actually were.
fn numbered(suggestions: &[String]) -> String {
    suggestions
        .iter()
        .enumerate()
        .map(|(index, suggestion)| format!("{}) {suggestion}", index.saturating_add(1)))
        .collect::<Vec<_>>()
        .join("  ")
}

/// Map the acknowledged-write verdict onto this verb's exit contract.
/// [`ApplyVerdict::Unknown`] (which absorbs unnamed codes) is the only outcome
/// where bytes may have reached the tty: exit 3 and not retryable. Every other
/// arm wrote nothing and is safe to resubmit.
fn refusal_for_verdict(verdict: ApplyVerdict, label: &str) -> Refusal {
    match verdict {
        // Handled by the caller; folded in so the mapping stays total.
        ApplyVerdict::Acked => Refusal::new(
            codes::ANSWER_REFUSED,
            format!("{label}: internal error — a delivered answer read as a refusal"),
            "report this",
            crate::exit_codes::EXIT_FAILURE,
        ),
        ApplyVerdict::Unknown(message) => Refusal::new(
            codes::ANSWER_DELIVERY_UNKNOWN,
            format!("{label}: the answer's delivery could not be confirmed ({message})"),
            "do NOT retry. Some, all, or none of the answer reached the pane. Read it \
             with `phux snapshot` and decide from what you see",
            crate::exit_codes::EXIT_PARTIAL_VIEW,
        ),
        ApplyVerdict::Busy(message) => Refusal::new(
            codes::ANSWER_REFUSED,
            format!("{label}: the pane's acknowledged input is busy ({message})"),
            "nothing was typed — this refusal happens before the write. Back off and run \
             the same command again",
            crate::exit_codes::EXIT_FAILURE,
        ),
        ApplyVerdict::NotWritten(message) => Refusal::new(
            codes::ANSWER_REFUSED,
            format!("{label}: nothing was typed ({message})"),
            "nothing reached the pane, so re-running this command — even with a fresh \
             operation id — cannot type the answer twice",
            crate::exit_codes::EXIT_FAILURE,
        ),
        ApplyVerdict::NotFound(message) => Refusal::new(
            codes::ANSWER_REFUSED,
            format!("{label}: {message}"),
            "nothing was typed. The pane went away between resolving it and writing",
            crate::exit_codes::EXIT_FAILURE,
        ),
        ApplyVerdict::Refused(reason) => Refusal::new(
            codes::ANSWER_REFUSED,
            format!("{label}: the server refused the answer ({reason})"),
            "nothing was typed — this refusal is decided before any byte reaches the \
             pane, so retrying the identical command cannot change the answer",
            crate::exit_codes::EXIT_USAGE,
        ),
    }
}

/// `phux agent answer TARGET --id ID (--choice N | --text T)
/// [--allow-unlisted] [--json]`.
pub(super) fn run_agent_answer(
    target: &str,
    id: &str,
    choice: Option<usize>,
    text: Option<&str>,
    allow_unlisted: bool,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    // A usage mistake must not need a running server to diagnose.
    if choice.is_none() && text.is_none() {
        return no_answer_refusal().emit(json);
    }
    let selector = match parse_selector(Some(target)) {
        Ok(selector) => selector,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    rt.block_on(async move {
        let pane = match resolve_target_for_input(&socket_path, &selector, Verb::AgentAnswer, json)
            .await
        {
            Ok(pane) => pane,
            Err(code) => return code,
        };
        let label = phux_client::selector::format_terminal_id(&pane);
        if !pane.is_local() {
            return Refusal::new(
                json_err::codes::SATELLITE_TARGET,
                format!("{label} is on a federation satellite; answers are local-only"),
                "nothing was typed. Acknowledged input batches do not route across a \
                 hub; run `phux agent answer` on the satellite's own server",
                crate::exit_codes::EXIT_USAGE,
            )
            .emit(json);
        }

        let mut conn = match Connection::connect(&socket_path).await {
            Ok(conn) => conn,
            Err(err) => {
                return json_err::report_no_server(json, &err, &socket_path, "agent answer");
            }
        };
        // An unknown APPLY_INPUT tag would be silence on an older server.
        if !supports_acknowledged_input(&conn) {
            return Refusal::new(
                json_err::codes::SERVER_TOO_OLD,
                "this server does not accept acknowledged input batches, which is how an \
                 answer is delivered",
                "nothing was typed. Upgrade the server (`phux update`), or answer the \
                 pane manually with `phux send-keys`",
                crate::exit_codes::EXIT_USAGE,
            )
            .emit(json);
        }

        // The liveness read and the write are consecutive frames on one
        // connection, so nothing this client sends interleaves between them.
        // Degradation is irrelevant: the pane is already known to be local.
        let snapshot = match phux_client::state::get_state_on(&mut conn).await {
            Ok(view) => view.into_snapshot_ignoring_degradation(),
            Err(err) => {
                return json_err::report_no_server(json, &err, &socket_path, "agent answer");
            }
        };
        if !snapshot.resources.iter().any(|info| info.id == pane) {
            return Refusal::new(
                json_err::codes::NO_SUCH_TARGET,
                format!("{label} is gone: it left between resolving the target and reading it"),
                "nothing was typed",
                crate::exit_codes::EXIT_FAILURE,
            )
            .emit(json);
        }
        // The sentinel lives in the live OSC title, which `GET_SCREEN`
        // carries; the snapshot's `title` is the user-set one.
        let title = match phux_client::snapshot::get_screen_on(&mut conn, 2, pane.clone()).await {
            Ok(screen) => screen.title,
            Err(err) => {
                return json_err::report_no_server(json, &err, &socket_path, "agent answer");
            }
        };

        let marker = match live_ask(title.as_deref(), id) {
            Ok(marker) => marker,
            Err(refusal) => return refusal.emit(json),
        };
        let (answer, source) = match select_answer(&marker, choice, text, allow_unlisted) {
            Ok(pair) => pair,
            Err(refusal) => return refusal.emit(json),
        };

        let uuid = uuid::Uuid::new_v4();
        let Some(operation_id) = InputOperationId::new(uuid.into_bytes()) else {
            return Refusal::new(
                codes::ANSWER_REFUSED,
                "could not mint an operation id",
                "report this: a v4 UUID cannot be all-zero",
                crate::exit_codes::EXIT_FAILURE,
            )
            .emit(json);
        };
        // `Busy` is not retried (unlike `agent prompt`): the liveness check
        // would go stale while waiting, so the caller re-runs the command.
        let verdict = deliver_answer(&mut conn, &pane, operation_id, 1, &answer).await;
        drop(conn);
        match verdict {
            Ok(ApplyVerdict::Acked) => {
                report_answered(json, &label, &marker, &answer, source, &uuid)
            }
            Ok(verdict) => refusal_for_verdict(verdict, &label).emit(json),
            Err(err) => json_err::report_no_server(json, &err, &socket_path, "agent answer"),
        }
    })
}

/// Report a delivered answer.
fn report_answered(
    json: bool,
    label: &str,
    marker: &AskMarker,
    answer: &str,
    source: AnswerSource,
    operation: &uuid::Uuid,
) -> ExitCode {
    if !json {
        outln!("answered {} on {label}: {answer}", marker.id);
        return ExitCode::SUCCESS;
    }
    let document = serde_json::json!({
        "schema_version": 1,
        "terminal": label,
        "ask": {
            "id": marker.id,
            "question": marker.question,
            "suggestions": marker.suggestions,
        },
        "answer": answer,
        "source": source.as_str(),
        "operation_id": operation.to_string(),
        "delivered": true,
    });
    match serde_json::to_string_pretty(&document) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(err) => json_err::emit(
            true,
            &json_err::CliError::new(
                json_err::codes::JSON_SERIALIZE,
                format!("could not render agent answer JSON: {err}"),
                "report this: a document of strings cannot fail to serialize",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
    }
}

#[cfg(test)]
mod tests;
