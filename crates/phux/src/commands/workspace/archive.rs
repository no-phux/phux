use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::connection::Connection;
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{Command, CommandResult};
use phux_server::runtime::default_socket_path;

use crate::commands::agent::{
    AgentSessionRecord, PreparedAgentSession, fetch_record_index, persist_record, prepare,
};
use crate::commands::new::{create_session_via_metadata, preflight_atomic_agent_session_create};
use crate::commands::{cli_runtime, partial, report_no_server};

mod model;
mod snapshot;

use model::{ARCHIVE_SCHEMA_VERSION, RestoreSummary, parse_archive, restore_plan};
use snapshot::archive_from_snapshot;

pub(super) fn run_save(socket: Option<PathBuf>, output: Option<&PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let (snapshot, degradation) = match rt.block_on(phux_client::state::get_state(&socket_path)) {
        Ok(view) => view.into_parts(),
        Err(err) => return report_no_server(&err, &socket_path, "workspace save"),
    };
    // A save writes a file the user will restore *later*, so an incomplete
    // capture is the one degradation that outlives the command: panes on an
    // unreachable satellite are simply not in the archive, and nothing at
    // restore time can tell they were meant to be. Still not a failure —
    // refusing to snapshot this laptop because a remote box is down would be
    // worse — but it has to be said out loud before the file lands.
    partial::warn_partial_view("workspace save", &degradation);
    let agent_sessions = match rt.block_on(fetch_record_index(&socket_path, &snapshot)) {
        Ok(index) => index,
        Err(err) => return fail(&format!("could not capture native agent sessions: {err}")),
    };
    let confirmation = match rt.block_on(phux_client::state::get_state(&socket_path)) {
        Ok(view) => view.into_snapshot_ignoring_degradation(),
        Err(err) => return report_no_server(&err, &socket_path, "workspace save"),
    };
    if !same_local_terminals(&snapshot, &confirmation) {
        return fail(
            "workspace changed while native agent sessions were captured; retry workspace save",
        );
    }
    let archive = archive_from_snapshot(&snapshot, &agent_sessions);
    let rendered = match serde_json::to_string_pretty(&archive) {
        Ok(rendered) => rendered,
        Err(err) => return fail(&format!("could not render workspace archive: {err}")),
    };
    if let Some(path) = output {
        match fs::write(path, rendered) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => fail(&format!("could not write {}: {err}", path.display())),
        }
    } else {
        outln!("{rendered}");
        ExitCode::SUCCESS
    }
}

pub(super) fn run_restore(archive_path: &Path, socket: Option<PathBuf>) -> ExitCode {
    let input = match read_archive_text(archive_path) {
        Ok(input) => input,
        Err(err) => return fail(&err),
    };
    let archive = match parse_archive(&input) {
        Ok(archive) => archive,
        Err(err) => return fail(&err),
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let existing = match rt.block_on(fetch_existing_sessions(&socket_path)) {
        Ok(existing) => existing,
        Err(code) => return code,
    };
    let plan = match restore_plan(&archive, &existing) {
        Ok(plan) => plan,
        Err(err) => return fail(&err),
    };
    let mut prepared_creates = Vec::with_capacity(plan.creates.len());
    for create in plan.creates {
        let prepared = match create.agent_session.as_ref() {
            Some(record) => match prepare_archived_agent(record, create.cwd.as_deref()) {
                Ok(prepared) => Some(prepared),
                Err(err) => return fail(&err),
            },
            None => None,
        };
        prepared_creates.push((create, prepared));
    }
    if prepared_creates
        .iter()
        .any(|(_, prepared)| prepared.is_some())
        && let Err(code) = rt.block_on(preflight_atomic_agent_session_create(&socket_path))
    {
        return code;
    }

    let mut restored = Vec::with_capacity(prepared_creates.len());
    for (create, prepared) in prepared_creates {
        let command = prepared
            .as_ref()
            .map_or(create.command, |session| Some(session.argv.clone()));
        let env = prepared
            .as_ref()
            .map_or_else(BTreeMap::new, |session| session.env.clone());
        let cwd = prepared.as_ref().map_or(create.cwd, |session| {
            Some(session.cwd.display().to_string())
        });
        let agent_session = match prepared
            .as_ref()
            .map(|session| session.record.encode())
            .transpose()
        {
            Ok(record) => record,
            Err(err) => return fail(&err),
        };
        let agent_session_preflighted = prepared.is_some();
        match rt.block_on(create_session_via_metadata(
            &socket_path,
            &create.name,
            command,
            cwd,
            env,
            agent_session,
            agent_session_preflighted,
        )) {
            Ok(pane_id) => {
                if let Some(prepared) = &prepared
                    && let Err(err) =
                        rt.block_on(confirm_restored_agent(&socket_path, pane_id, prepared))
                {
                    return fail(&err);
                }
                restored.push(create.name);
            }
            Err(code) => return code,
        }
    }
    let summary = RestoreSummary {
        schema_version: ARCHIVE_SCHEMA_VERSION,
        restored,
        skipped_existing: plan.skipped_existing,
    };
    match serde_json::to_string_pretty(&summary) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(err) => fail(&format!("could not render restore summary: {err}")),
    }
}

fn prepare_archived_agent(
    archived: &model::WorkspaceAgentSession,
    cwd: Option<&str>,
) -> Result<PreparedAgentSession, String> {
    let record = AgentSessionRecord::new(
        &archived.plugin_id,
        &archived.integration_id,
        &archived.native_id,
    )?;
    let workspace_cwd = match cwd {
        Some(cwd) => PathBuf::from(cwd),
        None => std::env::current_dir()
            .map_err(|err| format!("could not resolve restore working directory: {err}"))?,
    };
    let resolved = phux_plugin::resolve_launch(
        &phux_config::loader::config_path(),
        &record.integration_id,
        &[],
        &workspace_cwd,
    )
    .map_err(|err| {
        format!(
            "cannot restore native agent session for integration '{}': {err}",
            record.integration_id
        )
    })?;
    if resolved.plugin_id != record.plugin_id {
        return Err(format!(
            "cannot restore native agent session '{}': integration '{}' now resolves to plugin '{}', not owning plugin '{}'",
            record.native_id, record.integration_id, resolved.plugin_id, record.plugin_id
        ));
    }
    prepare(&resolved, &record.native_id)
}

async fn confirm_restored_agent(
    socket_path: &Path,
    pane_id: u64,
    prepared: &PreparedAgentSession,
) -> Result<(), String> {
    let local_id = u32::try_from(pane_id)
        .map_err(|_| format!("restored terminal id {pane_id} exceeds the local wire-id range"))?;
    let terminal = TerminalId::local(local_id);
    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(|err| format!("could not confirm restored agent session: {err}"))?;
    if let Err(err) = persist_record(&mut conn, &terminal, &prepared.record, 10).await {
        let cleanup = conn
            .request(
                13,
                Command::KillTerminal {
                    terminal_id: terminal,
                },
            )
            .await;
        let cleanup_note = match cleanup {
            Ok(reply) => match reply.into_parts().0 {
                CommandResult::Ok => "restored terminal removed".to_owned(),
                other => format!("cleanup returned {other:?}"),
            },
            Err(cleanup_err) => format!("cleanup failed: {cleanup_err}"),
        };
        return Err(format!(
            "restored agent session record could not be confirmed: {err}; {cleanup_note}"
        ));
    }
    Ok(())
}

fn read_archive_text(path: &Path) -> Result<String, String> {
    if path == Path::new("-") {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .map_err(|err| format!("could not read workspace archive from stdin: {err}"))?;
        return Ok(input);
    }
    fs::read_to_string(path)
        .map_err(|err| format!("could not read workspace archive {}: {err}", path.display()))
}

/// The session names already on the server, for restore's collision check.
///
/// `into_snapshot_ignoring_degradation`: this reads `sessions` only, and a
/// satellite's session list never enters the merge —
/// `handle_get_state_federated` discards it because its `u32` ids would
/// collide with the hub's. An unreachable satellite therefore cannot hide a
/// name this check would otherwise catch.
async fn fetch_existing_sessions(socket_path: &Path) -> Result<Vec<String>, ExitCode> {
    phux_client::state::get_state(socket_path)
        .await
        .map(|view| {
            view.into_snapshot_ignoring_degradation()
                .sessions
                .into_iter()
                .map(|session| session.name)
                .collect()
        })
        .map_err(|err| report_no_server(&err, socket_path, "workspace restore"))
}

fn fail(message: &str) -> ExitCode {
    eprintln!("phux: {message}");
    ExitCode::FAILURE
}

fn same_local_terminals(
    before: &phux_protocol::wire::info::SessionSnapshot,
    after: &phux_protocol::wire::info::SessionSnapshot,
) -> bool {
    let local_ids = |snapshot: &phux_protocol::wire::info::SessionSnapshot| {
        snapshot
            .panes
            .iter()
            .filter(|pane| matches!(pane.id, TerminalId::Local { .. }))
            .map(|pane| pane.id.clone())
            .collect::<HashSet<_>>()
    };
    local_ids(before) == local_ids(after)
}

#[cfg(test)]
mod tests {
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::info::{SessionSnapshot, TerminalInfo};

    use super::*;

    fn snapshot(ids: &[u32]) -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), TerminalId::local(1)).with_panes(
            ids.iter()
                .map(|id| TerminalInfo::new(TerminalId::local(*id), WindowId::new(1), 80, 24))
                .collect(),
        )
    }

    #[test]
    fn save_guard_rejects_reaped_or_new_local_terminals() {
        assert!(same_local_terminals(&snapshot(&[1, 2]), &snapshot(&[2, 1])));
        assert!(!same_local_terminals(&snapshot(&[1, 2]), &snapshot(&[1])));
        assert!(!same_local_terminals(
            &snapshot(&[1, 2]),
            &snapshot(&[1, 2, 3])
        ));
    }
}
