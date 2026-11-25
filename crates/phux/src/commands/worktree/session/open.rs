//! Idempotent worktree opening is a host ensure, never an exists/create race.
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::connection::Connection;
use phux_client::org::{self, OrgError};
use phux_protocol::wire::frame::{Committed, ExecState, LaunchSpec, SessionLifetime};

use crate::commands::server_target::ServerTarget;
use crate::commands::worktree::{binding_json, report, resolve_target, session_name_for};
use crate::commands::{attach, cli_runtime, ensure_socket_path_fits, new};

pub(in crate::commands::worktree) fn run_open(
    target: &str,
    repo: &Path,
    socket: Option<PathBuf>,
    attach: bool,
    json: bool,
) -> ExitCode {
    let (_root, entry) = match resolve_target(target, repo) {
        Ok(found) => found,
        Err(err) => return report(json, &err),
    };
    let name = session_name_for(&entry.path);
    let socket_path = socket.unwrap_or_else(phux_config::socket::default_socket_path);
    if let Err(code) = ensure_socket_path_fits(&socket_path) { return code; }
    if attach {
        if let Err(code) = attach::interactive_tty_preflight() { return code; }
        if let Err(code) = attach::nested_attach_guard(&socket_path) { return code; }
    }
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let server = ServerTarget::local(&socket_path);
    new::ensure_local_unseeded_server(&server, json);
    let (mut conn, receipt) = match rt.block_on(ensure_open_session(&server, &name, &entry.path)) {
        Ok(opened) => opened,
        Err(OrgError::Attach(err)) => return server.report_unreachable(json, &err, "worktree open"),
        Err(err) => {
            let error = crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::SESSION_CREATE_FAILED,
                format!("could not ensure worktree session '{name}': {err}"),
                "inspect `phux ls --json`; retain the original operation after an uncertain ensure",
            ).with_cause_code(err.code()).with_recovery(err.recovery());
            return crate::commands::json_err::emit(json, &error, 1);
        }
    };
    if attach {
        let selected = match rt.block_on(org::attach_target_for_receipt(&mut conn, &receipt)) {
            Ok(target) => target,
            Err(err) => return report_committed(json, &name, &receipt, &err),
        };
        let dial = server.dial();
        let predict = attach::predictive_config_for(&dial);
        let result = rt.block_on(attach::attach_with_reconnect(
            &dial, Some(Box::new(conn)), selected, predict, None, None,
        ));
        let exit = match result {
            Ok(end) => { attach::report_attach_end(end); ExitCode::SUCCESS }
            Err(err) => {
                error!("session '{name}' was ensured, but attach failed: {err}");
                hint!("Inspect `phux ls`; do not create another session to recover this attach.");
                ExitCode::FAILURE
            }
        };
        rt.shutdown_timeout(Duration::ZERO);
        return exit;
    }
    if !json && receipt.ensured_created == Some(false) {
        outln!("Session {name} is already open. Next: `phux attach {name}`.");
        return ExitCode::SUCCESS;
    }
    let terminal = match rt.block_on(seed_for_receipt(&mut conn, &receipt)) {
        Ok(Some(terminal)) => terminal,
        Ok(None) => return crate::commands::json_err::emit(json, &new::committed_session_error(&name, &receipt), 1),
        Err(err) => return report_committed(json, &name, &receipt, &err),
    };
    if json {
        crate::output::json(&binding_json(entry.branch.as_deref(), &entry.path, &name, u64::from(terminal)))
    } else {
        outln!("Created session {name} for {}. Next: `phux attach {name}`.", entry.path.display());
        ExitCode::SUCCESS
    }
}

async fn ensure_open_session(server: &ServerTarget, name: &str, cwd: &Path) -> Result<(Connection, Committed), OrgError> {
    let attrs = phux_client::project_identity::creation_attrs(cwd, &crate::commands::ls::local_hostname(), None);
    let launch = LaunchSpec::new(Vec::new()).with_cwd(Some(cwd.to_string_lossy().into_owned()));
    let mut conn = server.connect().await.map_err(OrgError::Attach)?;
    let receipt = org::ensure_session(
        &mut conn, org::mint_operation(), name.to_owned(), SessionLifetime::Persistent,
        Some(Box::new(launch)), attrs,
    ).await?;
    Ok((conn, receipt))
}

async fn seed_for_receipt(conn: &mut Connection, receipt: &Committed) -> Result<Option<u32>, OrgError> {
    let (key, inventory) = org::session_inventory_for_receipt(conn, receipt).await?;
    if inventory.session(&key).and_then(|session| session.live).is_none() {
        return Ok(None);
    }
    Ok(inventory.members(&key)
        .filter(|record| !record.activation.as_ref().is_some_and(|activation| activation.state == ExecState::Failed))
        .filter_map(|record| record.live.as_ref().and_then(phux_protocol::ids::ResourceId::local_id))
        .min())
}

fn report_committed(json: bool, name: &str, receipt: &Committed, detail: &OrgError) -> ExitCode {
    let mut error = new::committed_session_error(name, receipt);
    error.message.push_str(&format!(": {detail}"));
    crate::commands::json_err::emit(json, &error, 1)
}

#[cfg(test)]
mod tests;
