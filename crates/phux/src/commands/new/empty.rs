//! `phux new --empty`: a session with no terminal yet.

use std::process::ExitCode;

use phux_protocol::wire::frame::{Committed, SessionLifetime};

use crate::commands::server_target::ServerTarget;
use crate::commands::{
    attach::report_attach_end, attach::run_attach_once, server::ensure_server_unseeded,
};

use super::create::{
    committed_session_error, report_create_refused, report_create_session_error,
};
use super::naming::resolve_session_name;

/// `phux new --empty [NAME]`: create an empty session, then attach to it. The
/// TUI renders its empty state, from which a new window starts a terminal.
pub(super) fn run_new_empty_attached(
    rt: &tokio::runtime::Runtime,
    target: &ServerTarget,
    requested: Option<String>,
) -> ExitCode {
    if let Some(path) = target.socket_path()
        && let Err(code) = super::super::attach::nested_attach_guard(path)
    {
        return code;
    }
    let name = resolve_session_name(rt, target, requested);
    ensure_local_unseeded_server(target, false);
    let session_target = match rt.block_on(async {
        let receipt = create_empty_session_via_metadata(target, &name, false).await?;
        let selected = async {
            let mut conn = target.connect().await.map_err(phux_client::org::OrgError::Attach)?;
            phux_client::org::attach_target_for_receipt(&mut conn, &receipt).await
        }.await;
        selected.map_err(|err| {
            let mut error = committed_session_error(&name, &receipt);
            error.message.push_str(&format!("; exact attach selection failed: {err}"));
            crate::commands::json_err::emit(false, &error, 1)
        })
    }) {
        Ok(target) => target,
        Err(code) => return code,
    };
    let dial = target.dial();
    let predict_cfg = super::super::attach::predictive_config_for(&dial);
    match rt.block_on(run_attach_once(
        &dial,
        session_target,
        predict_cfg,
    )) {
        Ok(attach_end) => {
            report_attach_end(attach_end);
            ExitCode::SUCCESS
        }
        Err(err) => {
            warn!("session '{name}' was created; inspect `phux ls` before retrying the attach");
            target.report_attach_failure(&err, &name);
            ExitCode::FAILURE
        }
    }
}

/// `phux new --empty --json -s NAME`: create an empty session without
/// attaching and print [`empty_session_json`].
pub(super) fn run_new_empty_json(
    rt: &tokio::runtime::Runtime,
    target: &ServerTarget,
    name: &str,
) -> ExitCode {
    ensure_local_unseeded_server(target, true);
    match rt.block_on(create_empty_session_via_metadata(target, name, true)) {
        Ok(_) => crate::output::json(&empty_session_json(name)),
        Err(code) => code,
    }
}

/// Make sure a local server is running for a create-without-attach (`phux
/// new --json`, `phux new --empty`, `phux worktree new --json`). A server
/// started here carries no seed session (ADR-0105): the requested session is
/// the only one, so a requested name of `default` does not collide with a
/// seed and no stray shell is left behind. Failure is only logged (never
/// prose under `--json`); the create reports, its JSON error carrying the
/// failure as `error.auto_start_error`.
pub(crate) fn ensure_local_unseeded_server(target: &ServerTarget, json: bool) {
    if let Some(path) = target.socket_path()
        && let Err(err) = ensure_server_unseeded(path, json)
    {
        tracing::debug!(error = %err, "auto-spawn failed on a create-without-attach path");
    }
}

/// The `phux new --empty --json` result document: the same keys as
/// [`new_session_json`], with `terminal_id` null because no terminal was
/// started, plus `empty` and `keep_empty` (the session is persistent), both
/// `true`.
pub(super) fn empty_session_json(session: &str) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "session": session,
        "terminal_id": null,
        "empty": true,
        "keep_empty": true,
    })
}

/// Create an empty, persistent session.
pub(crate) async fn create_empty_session_via_metadata(
    server: &ServerTarget,
    name: &str,
    json: bool,
) -> Result<Committed, ExitCode> {
    let mut conn = server.connect().await
        .map_err(|err| server.report_unreachable(json, &err, "new"))?;
    phux_client::org::create_session(
        &mut conn, phux_client::org::mint_operation(), name.to_owned(),
        SessionLifetime::Persistent, None,
    ).await.map_err(|err| {
        if let Some(refusal) = err.refusal() {
            report_create_refused(json, name, refusal)
        } else {
            report_create_session_error(server, json, phux_client::session::CreateSessionError::Org(err))
        }
    })
}
