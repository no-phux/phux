//! `phux agent set` / `clear` — write the structured `phux.agent/v1`
//! record (ADR-0040), and the pipelined read-back the detector consumes.
//!
//! The record is the stable agent-identity path: it rides the existing L3
//! `SET_METADATA` / `GET_METADATA` / `DELETE_METADATA` verbs (no wire
//! change), the server stores it opaquely, and every consumer — this CLI's
//! `agent list/show/explain`, the TUI sidebar, a future fleet dashboard —
//! reads the same bytes instead of re-deriving state from title or screen
//! substrings. See `docs/spec/L3.md` §3.7 for the normative schema.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::agent_meta::{
    AgentAttention, AgentMetaState, AgentRecord, TERMINAL_AGENT_KEY, parse_agent_record,
};
use phux_client::attach::connection::Connection;
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{FrameKind, Scope};
use phux_protocol::wire::info::SessionSnapshot;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, report_no_server, resolve_targets};

/// `phux agent set [TARGET] --name ... [--kind] [--state] [--attention]
/// [--session]` — declare the target pane's agent identity by writing the
/// whole `phux.agent/v1` record (last writer wins; no field merges).
#[allow(clippy::too_many_arguments, reason = "one flag per record field")]
pub(super) fn run_agent_set(
    target: Option<&str>,
    name: &str,
    kind: Option<&str>,
    state: Option<&str>,
    attention: Option<&str>,
    session: Option<&str>,
    socket: Option<PathBuf>,
) -> ExitCode {
    if name.trim().is_empty() {
        eprintln!("phux: agent --name must not be empty");
        return ExitCode::FAILURE;
    }
    let record = AgentRecord {
        name: name.trim().to_owned(),
        kind: kind.map(str::to_owned),
        // The clap value parsers restrict these to the v1 vocabulary, so
        // the open-enum From<String> fallback is unreachable here.
        state: state
            .map(|s| AgentMetaState::from(s.to_owned()))
            .unwrap_or_default(),
        attention: attention.map(|a| AgentAttention::from(a.to_owned())),
        session: session.map(str::to_owned),
    };
    with_target_pane(target, socket, "agent set", move |conn, pane| {
        Box::pin(async move {
            conn.send(&FrameKind::SetMetadata {
                request_id: 100,
                scope: Scope::Terminal(pane.clone()),
                key: TERMINAL_AGENT_KEY.to_owned(),
                value: record.encode(),
            })
            .await?;
            // The trailing GET is load-bearing (same as `phux tag`):
            // SET_METADATA has no reply frame, so without a round-trip the
            // process could exit before the server reads the SET. Frames
            // are ordered on the one connection, so the reply proves the
            // write landed; we print that confirmed value.
            let confirmed = get_record(conn, &pane, 101).await?;
            match confirmed {
                Some(rec) => outln!("{}", render_record(&pane, Some(&rec))),
                None => eprintln!("phux: agent record did not persist"),
            }
            Ok(())
        })
    })
}

/// `phux agent clear [TARGET]` — delete the target pane's `phux.agent/v1`
/// record; consumers fall back to OSC-title / screen heuristics.
pub(super) fn run_agent_clear(target: Option<&str>, socket: Option<PathBuf>) -> ExitCode {
    with_target_pane(target, socket, "agent clear", move |conn, pane| {
        Box::pin(async move {
            conn.send(&FrameKind::DeleteMetadata {
                request_id: 100,
                scope: Scope::Terminal(pane.clone()),
                key: TERMINAL_AGENT_KEY.to_owned(),
            })
            .await?;
            // Same load-bearing confirmation round-trip as `set`.
            let confirmed = get_record(conn, &pane, 101).await?;
            match confirmed {
                None => outln!("{}", render_record(&pane, None)),
                Some(_) => eprintln!("phux: agent record was not cleared"),
            }
            Ok(())
        })
    })
}

/// Resolve `target` to exactly one pane (focused-pane fallback, like
/// `agent show`) and run `body` against it on a fresh connection.
fn with_target_pane<F>(
    target: Option<&str>,
    socket: Option<PathBuf>,
    verb: &'static str,
    body: F,
) -> ExitCode
where
    F: for<'c> FnOnce(
        &'c mut Connection,
        TerminalId,
    ) -> std::pin::Pin<
        Box<dyn Future<Output = Result<(), phux_client::attach::AttachError>> + 'c>,
    >,
{
    let selector = match crate::commands::parse_selector(target) {
        Ok(selector) => selector,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let mut conn = match Connection::connect(&socket_path).await {
            Ok(conn) => conn,
            Err(err) => return report_no_server(&err, &socket_path, verb),
        };
        let snapshot = match phux_client::state::get_state_on(&mut conn).await {
            Ok(snapshot) => snapshot,
            Err(err) => return report_no_server(&err, &socket_path, verb),
        };
        let candidates = resolve_targets(&socket_path, &selector, &snapshot).await;
        let Some(pane) = crate::selector::pick_target_pane(&candidates, &snapshot.focused_pane)
        else {
            eprintln!("phux: no such target");
            return ExitCode::FAILURE;
        };
        match body(&mut conn, pane).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => report_no_server(&err, &socket_path, verb),
        }
    })
}

/// One `GET_METADATA` round-trip for `pane`'s agent record on `conn`.
async fn get_record(
    conn: &mut Connection,
    pane: &TerminalId,
    request_id: u32,
) -> Result<Option<AgentRecord>, phux_client::attach::AttachError> {
    conn.send(&FrameKind::GetMetadata {
        request_id,
        scope: Scope::Terminal(pane.clone()),
        key: TERMINAL_AGENT_KEY.to_owned(),
    })
    .await?;
    loop {
        match conn.recv().await? {
            FrameKind::MetadataValue {
                request_id: got,
                value,
            } if got == request_id => {
                return Ok(value.as_deref().and_then(parse_agent_record));
            }
            _ => {}
        }
    }
}

/// `SELECTOR<TAB>record-json` (or `SELECTOR<TAB>-` for a cleared record) —
/// one line, machine-splittable, mirroring `phux tag`'s confirmation output.
fn render_record(pane: &TerminalId, record: Option<&AgentRecord>) -> String {
    let selector = crate::selector::format_terminal_id(pane);
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

/// Fetch the `phux.agent/v1` index — `TerminalId` → decoded record — for
/// every pane in `snapshot`, over one fresh connection to `socket_path`.
///
/// One `GET_METADATA` per pane, pipelined (send all, then collect replies
/// by `request_id`), the same shape as `phux tag`'s `fetch_tag_index`. A
/// pane with no record, or bytes that fail the §3.7 validation, is simply
/// absent from the index. Best-effort: transport failure returns what was
/// collected so the caller degrades to heuristics instead of erroring.
pub(crate) async fn fetch_agent_index(
    socket_path: &std::path::Path,
    snapshot: &SessionSnapshot,
) -> std::collections::HashMap<TerminalId, AgentRecord> {
    let mut index = std::collections::HashMap::new();
    let ids: Vec<TerminalId> = snapshot.panes.iter().map(|p| p.id.clone()).collect();
    if ids.is_empty() {
        return index;
    }
    let Ok(mut conn) = Connection::connect(socket_path).await else {
        return index;
    };
    for (i, id) in ids.iter().enumerate() {
        let request_id = u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1);
        if conn
            .send(&FrameKind::GetMetadata {
                request_id,
                scope: Scope::Terminal(id.clone()),
                key: TERMINAL_AGENT_KEY.to_owned(),
            })
            .await
            .is_err()
        {
            return index;
        }
    }
    let mut remaining = ids.len();
    while remaining > 0 {
        match conn.recv().await {
            Ok(FrameKind::MetadataValue { request_id, value }) => {
                let Some(pos) = usize::try_from(request_id)
                    .ok()
                    .and_then(|r| r.checked_sub(1))
                else {
                    continue;
                };
                let Some(id) = ids.get(pos) else { continue };
                remaining -= 1;
                if let Some(record) = value.as_deref().and_then(parse_agent_record) {
                    index.insert(id.clone(), record);
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn satellite_set_and_clear_confirmations_use_canonical_selector() {
        let pane = TerminalId::satellite("region/@build", 7);
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
