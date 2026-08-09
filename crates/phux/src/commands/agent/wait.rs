//! `phux agent wait` — block until a pane's agent *transitions* into a
//! lifecycle state (ADR-0076 point 5).
//!
//! The verb exists because the level answer already has a home. `phux agent
//! show` answers "what state is this pane in right now"; this answers "tell me
//! when it changes into one of these", and those are different questions with
//! different failure modes. Conflating them is the bug this file is shaped to
//! prevent: `idle` is the detector's fail-safe fallthrough — the five shipped
//! manifests carry no positive `idle` rule at all — so a completion gate
//! satisfied by a level read of `idle` returns success on a crashed agent,
//! instantly, and on any pane with no manifest loaded. The predicate lives in
//! [`phux_client::agent_wait`]; this file is the surface over it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_client::agent_meta::{AgentMetaState, AgentRecord};
use phux_client::agent_wait::{
    AgentWaitError, AgentWaitResult, DEFAULT_UNTIL, parse_until, wait_for_agent_state,
};
use phux_client::attach::AttachError;
use phux_protocol::ids::TerminalId;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};

use super::model::AgentStateReport;

/// Version of the `agent wait` result document.
const RESULT_SCHEMA_VERSION: u8 = 1;

/// Poll-floor cadence for the `GET_METADATA` re-read. The same gap
/// `phux wait` uses, for the same reason: below human settle perception,
/// well above the per-read round-trip cost on a local UDS.
const POLL_INTERVAL: Duration = phux_client::wait::DEFAULT_POLL_INTERVAL;

/// A `--until` set nothing in this build can satisfy, refused up front.
///
/// `done` has zero producers in the shipped binary. No detection manifest
/// asserts it — `crates/phux-server/rules/*.toml` carry `working` and
/// `blocked` rules and nothing else — and the bundled Claude shim, the only
/// `done` writer phux ever shipped, now declares identity only. So
/// `--until done` on its own is a gate that can never open on a pane phux
/// itself instrumented, and letting it block for the full `--timeout` reports
/// "the agent did not finish in time" for what is really "nothing here can
/// ever say it did".
///
/// The check is on the *requested* set, not the resolved one: the default
/// set (`idle`, `blocked`, `done`) keeps `done` untouched, because `idle` and
/// `blocked` can both satisfy it and a third-party integration that does
/// publish `done` still benefits from it being in the set.
///
/// Unconditional rather than conditioned on the pane's kind, deliberately: the
/// pane's occupant is not known until after the selector resolves and the
/// record is read, and refusing before connecting is what makes this a usage
/// error rather than a late failure. That makes it wrong for a third-party
/// integration that genuinely writes `done`, so the diagnostic says so and
/// names the one-word fix.
fn unsatisfiable_until(requested: &[String], resolved: &[AgentMetaState]) -> bool {
    !requested.is_empty() && resolved == [AgentMetaState::Done]
}

/// The refusal [`unsatisfiable_until`] produces. Pure, so the wording that
/// has to carry the explanation is testable without a server.
fn unsatisfiable_until_error() -> json_err::CliError {
    json_err::CliError::new(
        json_err::codes::UNSATISFIABLE_WAIT,
        "`--until done` on its own is a wait this build can never satisfy: no shipped \
         detection manifest publishes 'done', and the bundled Claude shim declares identity \
         only",
        "add the state the turn actually lands in — `--until done --until idle` still fires \
         on 'done' — or drop `--until` for the default set (idle, blocked, done). If a \
         third-party integration on this pane does publish 'done', this refusal is wrong for \
         you: pair it with whichever state that agent passes through and the wait keeps its \
         'done' edge.",
    )
}

/// Resolve `--until` words into the target set, or the refusal that replaces
/// the wait.
///
/// Both refusals happen here, before a selector resolves and before a socket
/// opens, because both are usage errors: a word the vocabulary does not have,
/// and a set nothing in this build can satisfy (phux-w7z2.42).
///
/// `pub(super)` because `--until` now has a second surface: `agent prompt
/// --wait --until` re-implements this loop and so does not get the
/// unsatisfiable refusal. Pointing that call site here is the fix, and it is
/// one line.
pub(super) fn resolve_until(until: &[String]) -> Result<Vec<AgentMetaState>, json_err::CliError> {
    // clap's value parser already restricts the vocabulary, so an unparsed
    // word here would be a wiring bug rather than user input; it is still
    // reported as the usage error the ADR specifies rather than silently
    // dropped from the target set.
    let mut targets: Vec<AgentMetaState> = Vec::with_capacity(until.len());
    for word in until {
        let Some(state) = parse_until(word) else {
            return Err(json_err::CliError::new(
                json_err::codes::INVALID_SELECTOR,
                format!("'{word}' is not a waitable agent state"),
                "use one of: idle, working, blocked, done \
                 ('unknown' is departure, not a state to wait for)",
            ));
        };
        if !targets.contains(&state) {
            targets.push(state);
        }
    }
    if targets.is_empty() {
        targets.extend_from_slice(DEFAULT_UNTIL);
    }
    if unsatisfiable_until(until, &targets) {
        return Err(unsatisfiable_until_error());
    }
    Ok(targets)
}

/// `phux agent wait [TARGET] [--until STATE]... [--timeout SECS] [--json]`.
pub(super) fn run_agent_wait(
    target: Option<&str>,
    until: &[String],
    timeout: Option<u64>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let targets = match resolve_until(until) {
        Ok(targets) => targets,
        Err(err) => return json_err::emit(json, &err, crate::exit_codes::EXIT_USAGE),
    };

    let selector = match parse_selector(target) {
        Ok(selector) => selector,
        Err(code) => return code,
    };
    let timeout = timeout.map(Duration::from_secs);
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    rt.block_on(async move {
        let terminal = match resolve_target(&socket_path, &selector, "agent wait", json).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let outcome =
            wait_for_agent_state(&socket_path, &terminal, &targets, timeout, POLL_INTERVAL).await;
        let result = match outcome {
            Ok(result) => result,
            Err(AgentWaitError::NoRecord) => {
                return json_err::emit(
                    json,
                    &json_err::CliError::new(
                        json_err::codes::NO_AGENT_RECORD,
                        format!(
                            "{} declares no phux.agent/v1 record, so it has no agent \
                             lifecycle to wait on",
                            crate::selector::format_terminal_id(&terminal)
                        ),
                        "declare one with `phux agent set <TARGET> --name ...`, or run \
                         `phux agent install-claude` so the agent publishes its own; \
                         `phux wait` waits on screen content instead",
                    ),
                    crate::exit_codes::EXIT_USAGE,
                );
            }
            Err(AgentWaitError::Departed {
                from,
                reason,
                last_record,
            }) => {
                let who = last_record
                    .as_ref()
                    .map_or_else(|| "the agent".to_owned(), |rec| format!("'{}'", rec.name));
                return json_err::emit(
                    json,
                    &json_err::CliError::new(
                        json_err::codes::AGENT_DEPARTED,
                        format!(
                            "{who} departed from '{}' while waiting: {}",
                            from.as_str(),
                            reason.as_str()
                        ),
                        "a departure is not a completion — the agent went away rather \
                         than settling; inspect the pane with `phux agent explain` or \
                         `phux snapshot`",
                    ),
                    crate::exit_codes::EXIT_FAILURE,
                );
            }
            Err(AgentWaitError::Transport(err @ AttachError::Io(_))) => {
                return json_err::report_no_server(json, &err, &socket_path, "agent wait");
            }
            Err(AgentWaitError::Transport(err)) => {
                return json_err::emit(
                    json,
                    &json_err::CliError::new(
                        json_err::codes::TRANSPORT,
                        format!("agent wait failed: {err}"),
                        "run `phux doctor` for a health check",
                    ),
                    crate::exit_codes::EXIT_FAILURE,
                );
            }
        };

        // Detection provenance: which sources agreed, and how strongly. A
        // caller can tell "done, because a lifecycle hook said so" from
        // "done, because a screen rule guessed" — which is the whole reason
        // phux carries `sources[]` instead of an opaque status word.
        let provenance = provenance(&socket_path, &terminal, result.record.clone()).await;
        report(&terminal, &result, provenance.as_ref(), json)
    })
}

/// Render the outcome and pick the exit code.
fn report(
    terminal: &TerminalId,
    result: &AgentWaitResult,
    provenance: Option<&AgentStateReport>,
    json: bool,
) -> ExitCode {
    let label = crate::selector::format_terminal_id(terminal);
    if json {
        let document = serde_json::json!({
            "schema_version": RESULT_SCHEMA_VERSION,
            "terminal": label,
            "satisfied": result.satisfied(),
            "edge": result.edge.map(|edge| serde_json::json!({
                "from": edge.from.as_str(),
                "to": edge.to.as_str(),
                "via": edge.via.as_str(),
            })),
            "baseline": result.baseline.as_str(),
            "state": result.last.as_str(),
            "agent": result.record.as_ref().map(|record| serde_json::json!({
                "name": record.name,
                "kind": record.kind,
                "session": record.session,
            })),
            "observations": {
                "edges": result.edges,
                "pushes": result.pushes,
                "polls": result.polls,
            },
            // The detector's own evidence for the state the wait landed on.
            "detection": provenance,
        });
        match serde_json::to_string_pretty(&document) {
            Ok(rendered) => outln!("{rendered}"),
            Err(err) => {
                return json_err::emit(
                    true,
                    &json_err::CliError::new(
                        json_err::codes::JSON_SERIALIZE,
                        format!("could not render agent wait JSON: {err}"),
                        "report this: a document of strings and numbers cannot fail to \
                         serialize",
                    ),
                    crate::exit_codes::EXIT_FAILURE,
                );
            }
        }
    } else if let Some(edge) = result.edge {
        let name = result
            .record
            .as_ref()
            .map_or("agent", |record: &AgentRecord| record.name.as_str());
        let confidence =
            provenance.map_or_else(String::new, |report| format!("\t{:.2}", report.confidence));
        outln!(
            "{label}\t{name}\t{} -> {}\tvia {}{confidence}",
            edge.from.as_str(),
            edge.to.as_str(),
            edge.via.as_str(),
        );
    }

    if result.satisfied() {
        return ExitCode::SUCCESS;
    }
    // A timeout on a pane that was *already* resting in a target state is the
    // deliberate consequence of the edge rule, and the single most likely
    // thing to confuse a caller — so say it outright rather than leaving them
    // to infer it from a bare 124.
    if result.baseline == result.last {
        eprintln!(
            "phux: agent wait timed out; {label} held '{}' for the whole wait and never \
             transitioned ({} pushes, {} polls). A level read is `phux agent show` — this \
             verb reports transitions, so that a crashed agent resting at 'idle' cannot \
             pass for a finished one.",
            result.last.as_str(),
            result.pushes,
            result.polls,
        );
    } else {
        eprintln!(
            "phux: agent wait timed out; {label} last observed '{}' after {} transition(s)",
            result.last.as_str(),
            result.edges,
        );
    }
    ExitCode::from(crate::exit_codes::EXIT_WAIT_TIMEOUT)
}

/// The detector's report for `terminal`, with `record` folded in as the
/// highest-ranked source (ADR-0040).
///
/// Best effort: this runs *after* the wait has already produced its answer,
/// so a failure here degrades the result document by one optional field
/// rather than failing a wait that was satisfied.
async fn provenance(
    socket_path: &Path,
    terminal: &TerminalId,
    record: Option<AgentRecord>,
) -> Option<AgentStateReport> {
    let (snapshot, _degradation) = super::fetch_snapshot(socket_path, "agent wait")
        .await
        .ok()?;
    let pane = snapshot.panes.iter().find(|pane| pane.id == *terminal)?;
    let mut evidence = super::pane_evidence(socket_path, &snapshot, pane).await;
    evidence.record = record;
    Some(super::detect::infer_agent_state(
        &evidence,
        &super::config::configured_agents(),
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "tests")]

    use super::*;

    /// The `--until` vocabulary the CLI accepts is exactly the client-side
    /// one, and `unknown` is not in it: departure is not a state to wait for.
    #[test]
    fn until_vocabulary_matches_the_client_predicate() {
        for word in ["idle", "working", "blocked", "done"] {
            assert!(parse_until(word).is_some(), "{word} must be waitable");
        }
        assert!(parse_until("unknown").is_none());
    }

    /// The default set is the three ways a turn ends. `working` is spellable
    /// but never a default — waiting for a turn to *start* is a different
    /// intent and has to be asked for.
    #[test]
    fn the_default_until_set_is_the_end_states() {
        assert_eq!(
            DEFAULT_UNTIL,
            &[
                AgentMetaState::Idle,
                AgentMetaState::Blocked,
                AgentMetaState::Done
            ]
        );
    }

    /// phux-w7z2.42: `--until done` alone is refused as a usage error rather
    /// than blocking for the whole `--timeout` and exiting 124. Repeats
    /// collapse to the same set and are refused identically.
    #[test]
    fn until_done_alone_is_unsatisfiable() {
        let requested = vec!["done".to_owned()];
        assert!(unsatisfiable_until(&requested, &[AgentMetaState::Done]));

        let repeated = vec!["done".to_owned(), "done".to_owned()];
        assert!(unsatisfiable_until(&repeated, &[AgentMetaState::Done]));
    }

    /// The refusal is narrow on purpose. The default set is untouched, and
    /// `done` paired with any other state stays a legitimate wait — a
    /// third-party integration that does publish `done` keeps its edge.
    #[test]
    fn done_alongside_another_state_and_the_default_set_are_not_refused() {
        // No `--until` at all: the default set, which contains `done`.
        assert!(!unsatisfiable_until(&[], DEFAULT_UNTIL));
        // `--until done --until idle`.
        let requested = vec!["done".to_owned(), "idle".to_owned()];
        assert!(!unsatisfiable_until(
            &requested,
            &[AgentMetaState::Done, AgentMetaState::Idle]
        ));
        // Every single-state wait other than `done` has a producer.
        for state in [
            AgentMetaState::Idle,
            AgentMetaState::Working,
            AgentMetaState::Blocked,
        ] {
            let requested = vec![format!("{state:?}").to_lowercase()];
            assert!(!unsatisfiable_until(&requested, &[state]), "{state:?}");
        }
    }

    /// The whole `--until` resolution, as the verb runs it: both refusals
    /// land before a socket opens, and everything else resolves to a set.
    #[test]
    fn resolve_until_refuses_exactly_the_two_usage_errors() {
        assert_eq!(
            resolve_until(&[]).ok().as_deref(),
            Some(DEFAULT_UNTIL),
            "no --until is the default set"
        );
        assert_eq!(
            resolve_until(&["done".to_owned(), "idle".to_owned()]).ok(),
            Some(vec![AgentMetaState::Done, AgentMetaState::Idle])
        );

        let err = resolve_until(&["done".to_owned()]).expect_err("done alone is refused");
        assert_eq!(err.code, json_err::codes::UNSATISFIABLE_WAIT);

        let err = resolve_until(&["finished".to_owned()]).expect_err("unknown word is refused");
        assert_eq!(err.code, json_err::codes::INVALID_SELECTOR);
    }

    /// The point of the refusal is the diagnostic: it has to say why nothing
    /// can satisfy the wait, admit the case where it is wrong, and name the
    /// fix. A bare code would be no better than the 124 it replaces.
    #[test]
    fn the_unsatisfiable_refusal_explains_itself_and_names_the_fix() {
        let err = unsatisfiable_until_error();
        assert_eq!(err.code, json_err::codes::UNSATISFIABLE_WAIT);
        assert!(err.message.contains("never satisfy"), "{}", err.message);
        assert!(
            err.message.contains("no shipped detection manifest"),
            "must name the reason: {}",
            err.message
        );
        assert!(
            err.remedy.contains("--until done --until idle"),
            "must name the fix: {}",
            err.remedy
        );
        assert!(
            err.remedy.contains("third-party"),
            "must admit who this refusal is wrong for: {}",
            err.remedy
        );
        // Refusals are usage errors, exit 2 — never the 124 they replace.
        let doc = json_err::error_document(&err, crate::exit_codes::EXIT_USAGE);
        assert_eq!(doc["exit_code"], 2);
        assert_eq!(doc["error"]["code"], "unsatisfiable_wait");
    }
}
