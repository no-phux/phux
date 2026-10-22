//! `phux agent set` / `clear` — write the structured `phux.agent/v1`
//! record (ADR-0040), and the pipelined read-back the detector consumes.
//!
//! The record rides the L3 metadata verbs and is stored opaquely; every
//! consumer reads the same bytes (`docs/agents.md#agent-states`).

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::agent_meta::{
    AgentAttention, AgentMetaState, AgentRecord, MAX_AGENT_RECORD_BYTES,
};
use phux_client::attach::connection::{Answer, Connection};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::info::SessionSnapshot;
use phux_server::runtime::default_socket_path;

use crate::commands::json_err::{self, CliError, codes};
use crate::commands::server_target::ServerTarget;
use crate::commands::{cli_runtime, partial, resolve_targets, warn_interleaved_degradation};
use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

/// `phux agent set [TARGET] --name ... [--kind] [--state] [--attention]
/// [--session] [--json]` — declare the target pane's agent identity by
/// writing the whole `phux.agent/v1` record (last writer wins; no field
/// merges). Exits non-zero unless the read-back confirms the write.
#[allow(clippy::too_many_arguments, reason = "one flag per record field")]
pub(super) fn run_agent_set(
    target: Option<&str>,
    name: &str,
    kind: Option<&str>,
    state: Option<&str>,
    attention: Option<&str>,
    session: Option<&str>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    if name.trim().is_empty() {
        return usage_error(json, "agent --name must not be empty");
    }
    let record = AgentRecord {
        name: name.trim().to_owned(),
        kind: kind.map(str::to_owned),
        state: state
            .map(|s| AgentMetaState::from(s.to_owned()))
            .unwrap_or_default(),
        attention: attention.map(|a| AgentAttention::from(a.to_owned())),
        session: session.map(str::to_owned),
    };
    let encoded = record.encode().len();
    if encoded > MAX_AGENT_RECORD_BYTES {
        return usage_error(
            json,
            &format!(
                "agent record is {encoded} bytes; the limit is {MAX_AGENT_RECORD_BYTES} \
                 (shorten --name, --kind or --session)"
            ),
        );
    }
    with_target_pane(target, socket, "agent set", json, move |conn, pane| {
        Box::pin(async move {
            // `set_record` reads back: SET_METADATA has no reply frame, so the
            // read-back proves the write landed.
            let (answer, degradation) =
                phux_client::agent_record::set_record(conn, 100, &pane, &record).await?;
            warn_interleaved_degradation(&degradation);
            Ok(report_outcome(json, Write::Set, &pane, answer))
        })
    })
}

/// `phux agent clear [TARGET] [--json]` — delete the target pane's
/// `phux.agent/v1` record; consumers fall back to OSC-title / screen
/// heuristics. Exits non-zero unless the read-back confirms the delete.
pub(super) fn run_agent_clear(
    target: Option<&str>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    with_target_pane(target, socket, "agent clear", json, move |conn, pane| {
        Box::pin(async move {
            // Same load-bearing confirmation round-trip as `set`.
            let (answer, degradation) =
                phux_client::agent_record::clear_record(conn, 100, &pane).await?;
            warn_interleaved_degradation(&degradation);
            Ok(report_outcome(json, Write::Clear, &pane, answer))
        })
    })
}

/// Which write a read-back is confirming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Write {
    Set,
    Clear,
}

impl Write {
    const fn action(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Clear => "clear",
        }
    }
}

/// Print the confirmed record, or report a write the read-back could not
/// confirm as `agent_record_unconfirmed` (exit 1).
fn report_outcome(
    json: bool,
    write: Write,
    pane: &ResourceId,
    answer: Answer<Option<AgentRecord>>,
) -> ExitCode {
    let selector = phux_client::selector::format_terminal_id(pane);
    let message = match (write, answer) {
        (Write::Set, Ok(Some(record))) => return print_confirmed(json, write, pane, Some(&record)),
        (Write::Clear, Ok(None)) => return print_confirmed(json, write, pane, None),
        (Write::Set, Ok(None)) => format!("agent record for {selector} did not persist"),
        (Write::Clear, Ok(Some(_))) => format!("agent record for {selector} was not cleared"),
        // The server declined the read-back: the write is unknown.
        (_, Err(refusal)) => format!(
            "agent {} on {selector} could not be confirmed: {refusal}",
            write.action()
        ),
    };
    json_err::emit(
        json,
        &CliError::new(
            codes::AGENT_RECORD_UNCONFIRMED,
            message,
            format!(
                "check with `phux agent show {selector}`, then retry; run `phux doctor` if \
                 writes keep failing"
            ),
        ),
        EXIT_FAILURE,
    )
}

/// The confirmation: the `SELECTOR<TAB>record-json` line, or under `json`
/// the `{schema_version, action, terminal, record}` document the MCP
/// `phux_agent_set` / `phux_agent_clear` tools return.
fn print_confirmed(
    json: bool,
    write: Write,
    pane: &ResourceId,
    record: Option<&AgentRecord>,
) -> ExitCode {
    if !json {
        outln!("{}", render_record(pane, record));
        return ExitCode::SUCCESS;
    }
    crate::output::json(&record_document(write, pane, record))
}

fn record_document(
    write: Write,
    pane: &ResourceId,
    record: Option<&AgentRecord>,
) -> serde_json::Value {
    let record = record
        .and_then(|record| serde_json::from_slice(&record.encode()).ok())
        .unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "schema_version": 1,
        "action": write.action(),
        "terminal": phux_client::selector::format_terminal_id(pane),
        "record": record,
    })
}

fn usage_error(json: bool, message: &str) -> ExitCode {
    json_err::emit(
        json,
        &CliError::new(codes::USAGE_ERROR, message, "see `phux agent set --help`"),
        EXIT_USAGE,
    )
}

/// Resolve `target` to exactly one pane (focused-pane fallback, like
/// `agent show`) and run `body` against it on a fresh connection; `body`
/// decides the exit code. Failures before `body` report on `json`'s channel.
pub(super) fn with_target_pane<F>(
    target: Option<&str>,
    socket: Option<PathBuf>,
    verb: &'static str,
    json: bool,
    body: F,
) -> ExitCode
where
    F: for<'c> FnOnce(
        &'c mut Connection,
        ResourceId,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = Result<ExitCode, phux_client::attach::AttachError>> + 'c>,
    >,
{
    let selector = match target.map(phux_client::selector::parse).transpose() {
        Ok(selector) => selector.unwrap_or(phux_client::selector::Selector::Current),
        Err(err) => {
            let raw = target.unwrap_or_default();
            return json_err::emit(
                json,
                &CliError::new(
                    codes::INVALID_SELECTOR,
                    format!("invalid target '{raw}': {err}"),
                    "selector forms: NAME, NAME:WIN, NAME:WIN.PANE, @ID, `.`, #TAG, %NAME",
                ),
                EXIT_FAILURE,
            );
        }
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let mut conn = match ServerTarget::local(&socket_path).connect().await {
            Ok(conn) => conn,
            Err(err) => return json_err::report_no_server(json, &err, &socket_path, verb),
        };
        let (snapshot, degradation) = match phux_client::state::get_state_on(&mut conn).await {
            Ok(view) => view.into_parts(),
            Err(err) => return json_err::report_no_server(json, &err, &socket_path, verb),
        };
        // `%name` resolves to exactly one pane or refuses (ADR-0075 point 3).
        let candidates = match resolve_targets(&socket_path, &selector, &snapshot).await {
            Ok(candidates) => candidates,
            Err(err) => return crate::commands::report_agent_resolve_error(json, &err, false),
        };
        let Some(pane) =
            phux_client::selector::pick_target_pane(&candidates, &snapshot.focused_resource)
        else {
            // A miss under a partial fleet view is unresolved, not absent.
            return partial::report_target_miss_for(json, target, &degradation);
        };
        // Resolved, but from a narrower world than the user assumed.
        partial::warn_partial_view(verb, &degradation);
        match body(&mut conn, pane).await {
            Ok(code) => code,
            Err(err) => json_err::report_no_server(json, &err, &socket_path, verb),
        }
    })
}

/// `SELECTOR<TAB>record-json` (or `SELECTOR<TAB>-` for a cleared record) —
/// one line, machine-splittable, mirroring `phux tag`'s confirmation output.
fn render_record(pane: &ResourceId, record: Option<&AgentRecord>) -> String {
    let selector = phux_client::selector::format_terminal_id(pane);
    record.map_or_else(
        || format!("{selector}\t-"),
        |rec| {
            format!(
                "{selector}\t{}",
                String::from_utf8(rec.encode()).unwrap_or_default()
            )
        },
    )
}

/// Fetch the `phux.agent/v1` index — `ResourceId` → decoded record — for
/// every pane in `snapshot`. Best effort: a pane with no valid record is
/// absent, and a transport failure returns what was collected.
pub(crate) async fn fetch_agent_index(
    socket_path: &std::path::Path,
    snapshot: &SessionSnapshot,
) -> std::collections::HashMap<ResourceId, AgentRecord> {
    let mut notices = Vec::new();
    let index = phux_client::agent_record::fetch_index(socket_path, snapshot, &mut notices).await;
    for message in &notices {
        warn!("partial results — {message}");
    }
    index
}

#[cfg(test)]
mod tests {
    use phux_client::attach::connection::Refusal;
    use phux_protocol::wire::frame::ErrorCode;

    use super::*;

    fn record() -> AgentRecord {
        AgentRecord {
            name: "codex".to_owned(),
            kind: None,
            state: AgentMetaState::Working,
            attention: None,
            session: None,
        }
    }

    /// phux-jqlh0.7: a write the read-back cannot confirm exits 1, whether
    /// it did not land or the server refused the read; only a confirmed
    /// write exits 0.
    #[test]
    fn an_unconfirmed_set_or_clear_exits_non_zero() {
        let pane = ResourceId::local(7);
        let refused = || {
            Err(Refusal {
                code: ErrorCode::InvalidCommand,
                message: "no".to_owned(),
            })
        };
        for json in [false, true] {
            let failure = ExitCode::from(EXIT_FAILURE);
            assert_eq!(report_outcome(json, Write::Set, &pane, Ok(None)), failure);
            assert_eq!(report_outcome(json, Write::Set, &pane, refused()), failure);
            assert_eq!(
                report_outcome(json, Write::Clear, &pane, Ok(Some(record()))),
                failure
            );
            assert_eq!(
                report_outcome(json, Write::Clear, &pane, refused()),
                failure
            );
            assert_eq!(
                report_outcome(json, Write::Set, &pane, Ok(Some(record()))),
                ExitCode::SUCCESS
            );
            assert_eq!(
                report_outcome(json, Write::Clear, &pane, Ok(None)),
                ExitCode::SUCCESS
            );
        }
    }

    /// The `--json` confirmation is the document the MCP tools return.
    #[test]
    fn the_json_confirmation_matches_the_mcp_document() {
        let pane = ResourceId::local(7);
        let set = record_document(Write::Set, &pane, Some(&record()));
        assert_eq!(set["schema_version"], 1);
        assert_eq!(set["action"], "set");
        assert_eq!(set["terminal"], "@7");
        assert_eq!(set["record"]["name"], "codex");
        let clear = record_document(Write::Clear, &pane, None);
        assert_eq!(clear["action"], "clear");
        assert!(clear["record"].is_null());
    }

    #[test]
    fn satellite_set_and_clear_confirmations_use_canonical_selector() {
        let pane = ResourceId::satellite("region/@build", 7);
        assert_eq!(render_record(&pane, None), "region/@build/@7\t-");

        let record = AgentRecord {
            name: "codex".to_owned(),
            kind: None,
            state: AgentMetaState::Working,
            attention: None,
            session: None,
        };
        let rendered = render_record(&pane, Some(&record));
        assert!(rendered.starts_with("region/@build/@7\t{"));
        assert!(rendered.contains("\"name\":\"codex\""));
    }
}
