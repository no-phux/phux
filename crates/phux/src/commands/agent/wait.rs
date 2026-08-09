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

/// Stable error codes this verb reports.
///
/// These belong in `crate::commands::json_err::codes` alongside the rest of
/// the closed vocabulary; they are declared here only because that module is
/// owned by a concurrent change in this wave. Fold them in when the two land.
mod codes {
    /// The pane declares no `phux.agent/v1` record, so it has no agent
    /// lifecycle to wait on.
    pub(super) const NO_AGENT_RECORD: &str = "no_agent_record";
    /// The agent went away mid-wait (record deleted, or its state withdrawn
    /// to `unknown`). Not success and not a timeout.
    pub(super) const AGENT_DEPARTED: &str = "agent_departed";
}

/// `phux agent wait [TARGET] [--until STATE]... [--timeout SECS] [--json]`.
pub(super) fn run_agent_wait(
    target: Option<&str>,
    until: &[String],
    timeout: Option<u64>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    // clap's value parser already restricts the vocabulary, so an unparsed
    // word here would be a wiring bug rather than user input; it is still
    // reported as the usage error the ADR specifies rather than silently
    // dropped from the target set.
    let mut targets: Vec<AgentMetaState> = Vec::with_capacity(until.len());
    for word in until {
        let Some(state) = parse_until(word) else {
            return json_err::emit(
                json,
                &json_err::CliError::new(
                    json_err::codes::INVALID_SELECTOR,
                    format!("'{word}' is not a waitable agent state"),
                    "use one of: idle, working, blocked, done \
                     ('unknown' is departure, not a state to wait for)",
                ),
                crate::exit_codes::EXIT_USAGE,
            );
        };
        if !targets.contains(&state) {
            targets.push(state);
        }
    }
    if targets.is_empty() {
        targets.extend_from_slice(DEFAULT_UNTIL);
    }

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
                        codes::NO_AGENT_RECORD,
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
                        codes::AGENT_DEPARTED,
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
}
