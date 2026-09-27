//! `phux agent wait` — block until a pane's agent *transitions* into a
//! lifecycle state (ADR-0076 point 5).
//!
//! `phux agent show` is the level read; this waits for a transition, because
//! `idle` is the detector's fail-safe and a level match would pass a crashed
//! agent. The predicate lives in [`phux_client::agent_wait`].

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_client::agent_meta::{AgentMetaState, AgentRecord};
use phux_client::agent_wait::{
    AgentWaitError, AgentWaitResult, DEFAULT_UNTIL, FleetAgentWaitResult, parse_until,
    wait_for_agent_state, wait_for_any_agent_state,
};
use phux_client::attach::AttachError;
use phux_protocol::ids::ResourceId;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};

use super::model::AgentStateReport;

/// Version of the `agent wait` result document.
const RESULT_SCHEMA_VERSION: u8 = 1;

/// Poll-floor cadence for the `GET_METADATA` re-read (same as `phux wait`).
const POLL_INTERVAL: Duration = phux_client::wait::DEFAULT_POLL_INTERVAL;

/// Resolve `--until` words into the target set (shared with `agent prompt`),
/// or the refusal that replaces the wait.
pub(super) fn resolve_until(until: &[String]) -> Result<Vec<AgentMetaState>, json_err::CliError> {
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
    Ok(targets)
}

/// The refusal for a satellite target, or `None` for a local pane.
/// `phux.agent/v1` does not federate, so a hub would otherwise misreport a
/// live remote agent as `no_agent_record`.
fn satellite_refusal(terminal: &ResourceId) -> Option<json_err::CliError> {
    if terminal.is_local() {
        return None;
    }
    Some(json_err::CliError::new(
        json_err::codes::SATELLITE_TARGET,
        format!(
            "{} is on a federation satellite; phux.agent/v1 records are hub-local \
             and do not federate, so this hub can never observe its lifecycle",
            phux_client::selector::format_terminal_id(terminal)
        ),
        "run `phux agent wait` against the satellite's own server. \
         `phux watch <TARGET>` still streams that pane's agent events across the \
         hub — it is the metadata half that does not cross, not the event half",
    ))
}

/// `phux agent wait [TARGET] [--until STATE]... [--timeout SECS] [--json]`.
pub(super) fn run_agent_wait(
    target: Option<&str>,
    any: bool,
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
        if any {
            let outcome =
                wait_for_any_agent_state(&socket_path, &targets, timeout, POLL_INTERVAL).await;
            return match outcome {
                Ok(result) => report_any(&socket_path, &result, json).await,
                Err(err) => report_wait_error(err, &socket_path, json),
            };
        }
        let terminal = match resolve_target(&socket_path, &selector, "agent wait", json).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        if let Some(err) = satellite_refusal(&terminal) {
            return json_err::emit(json, &err, crate::exit_codes::EXIT_USAGE);
        }
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
                            phux_client::selector::format_terminal_id(&terminal)
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
            Err(err) => return report_wait_error(err, &socket_path, json),
        };

        // Detection provenance: which sources agreed, and how strongly.
        let provenance = provenance(&socket_path, &terminal, result.record.clone()).await;
        report(&terminal, &result, provenance.as_ref(), json)
    })
}

fn report_wait_error(err: AgentWaitError, socket_path: &Path, json: bool) -> ExitCode {
    match err {
        AgentWaitError::Transport(err @ AttachError::Io(_)) => {
            json_err::report_no_server(json, &err, socket_path, "agent wait")
        }
        AgentWaitError::Transport(err) => json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::TRANSPORT,
                format!("agent wait failed: {err}"),
                "run `phux doctor` for a health check",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
        AgentWaitError::NoRecord => json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::NO_AGENT_RECORD,
                "the selected pane declares no phux.agent/v1 record",
                "declare an agent record before waiting",
            ),
            crate::exit_codes::EXIT_USAGE,
        ),
        AgentWaitError::Departed { from, reason, .. } => json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::AGENT_DEPARTED,
                format!(
                    "agent departed from '{}' while waiting: {}",
                    from.as_str(),
                    reason.as_str()
                ),
                "a departure is not a completion",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
    }
}

async fn report_any(socket_path: &Path, result: &FleetAgentWaitResult, json: bool) -> ExitCode {
    let matched = result.matched.as_ref();
    let provenance = match matched {
        Some(matched) => {
            provenance(socket_path, &matched.terminal, Some(matched.record.clone())).await
        }
        None => None,
    };
    if json {
        let document = serde_json::json!({
            "schema_version": RESULT_SCHEMA_VERSION,
            "terminal": matched.map(|matched| phux_client::selector::format_terminal_id(&matched.terminal)),
            "satisfied": result.satisfied(),
            "edge": matched.map(|matched| serde_json::json!({
                "from": matched.edge.from.as_str(),
                "to": matched.edge.to.as_str(),
                "via": matched.edge.via.as_str(),
            })),
            "baseline": matched.map(|matched| matched.baseline.as_str()),
            "state": matched.map(|matched| matched.edge.to.as_str()),
            "agent": matched.map(|matched| serde_json::json!({
                "name": matched.record.name,
                "kind": matched.record.kind,
                "session": matched.record.session,
            })),
            "observations": {
                "agents": result.agents,
                "edges": result.edges,
                "pushes": result.pushes,
                "polls": result.polls,
            },
            "detection": provenance,
        });
        match serde_json::to_string_pretty(&document) {
            Ok(rendered) => outln!("{rendered}"),
            Err(err) => {
                return json_err::emit(
                    true,
                    &json_err::CliError::new(
                        json_err::codes::JSON_SERIALIZE,
                        format!("could not render fleet agent wait JSON: {err}"),
                        "report this serialization failure",
                    ),
                    crate::exit_codes::EXIT_FAILURE,
                );
            }
        }
    } else if let Some(matched) = matched {
        outln!(
            "{}\t{}\t{} -> {}\tvia {}",
            phux_client::selector::format_terminal_id(&matched.terminal),
            matched.record.name,
            matched.edge.from.as_str(),
            matched.edge.to.as_str(),
            matched.edge.via.as_str(),
        );
    }
    if result.satisfied() {
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "phux: agent wait --any timed out; {} agent(s) were tracked and no matching transition was observed",
            result.agents,
        );
        ExitCode::from(crate::exit_codes::EXIT_WAIT_TIMEOUT)
    }
}

/// Render the outcome and pick the exit code.
fn report(
    terminal: &ResourceId,
    result: &AgentWaitResult,
    provenance: Option<&AgentStateReport>,
    json: bool,
) -> ExitCode {
    let label = phux_client::selector::format_terminal_id(terminal);
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
    // A pane already resting in a target state times out by design; say so.
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
/// highest-ranked source (ADR-0040). Best effort: runs after the answer.
async fn provenance(
    socket_path: &Path,
    terminal: &ResourceId,
    record: Option<AgentRecord>,
) -> Option<AgentStateReport> {
    let (snapshot, _degradation) = super::fetch_snapshot(socket_path, "agent wait")
        .await
        .ok()?;
    let pane = snapshot
        .resources
        .iter()
        .find(|pane| pane.id == *terminal)?;
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

    /// The whole `--until` resolution, as the verb runs it: every known state
    /// resolves, including hook-produced `done`; unknown words are refused.
    #[test]
    fn resolve_until_accepts_done_and_refuses_unknown_words() {
        assert_eq!(
            resolve_until(&[]).ok().as_deref(),
            Some(DEFAULT_UNTIL),
            "no --until is the default set"
        );
        assert_eq!(
            resolve_until(&["done".to_owned(), "idle".to_owned()]).ok(),
            Some(vec![AgentMetaState::Done, AgentMetaState::Idle])
        );

        let err = resolve_until(&["finished".to_owned()]).expect_err("unknown word is refused");
        assert_eq!(err.code, json_err::codes::INVALID_SELECTOR);
    }

    #[test]
    fn any_is_accepted_without_a_target_and_conflicts_with_one() {
        assert!(crate::parse_cli(["phux", "agent", "wait", "--any"]).is_ok());
        assert!(
            crate::parse_cli(["phux", "agent", "wait", "@7", "--any"]).is_err(),
            "a fleet wait and one resolved target are different ownership scopes"
        );
    }

    /// A satellite target is refused up front, never read back from the hub's
    /// empty store as `no_agent_record`.
    #[test]
    fn a_satellite_target_is_refused_and_a_local_one_is_not() {
        assert!(satellite_refusal(&ResourceId::local(3)).is_none());

        let err = satellite_refusal(&ResourceId::Satellite {
            host: phux_protocol::ids::SatelliteHost::new("gpubox"),
            id: 7,
        })
        .expect("a satellite target is refused");
        assert_eq!(err.code, json_err::codes::SATELLITE_TARGET);
        let doc = json_err::error_document(&err, crate::exit_codes::EXIT_USAGE);
        assert_eq!(doc["exit_code"], 2);
        assert_eq!(doc["error"]["code"], "satellite_target");
    }
}
