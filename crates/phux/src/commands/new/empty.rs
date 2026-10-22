//! `phux new --empty`: a session with no terminal yet.

use std::process::ExitCode;

use phux_client::session::CreateEmptyOutcome;
use phux_protocol::wire::frame::AttachTarget;

use crate::commands::server_target::ServerTarget;
use crate::commands::{
    attach::report_attach_end, attach::run_attach_once, server::ensure_server_unseeded,
};

use super::create::{
    reject_duplicate_session_name, report_create_failed, report_create_refused,
    report_create_session_error, report_session_not_registered, warn_partial_results,
};
use super::naming::{choose_session_name, existing_session_names};

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
    let existing = existing_session_names(rt, target);
    let name = match choose_session_name(requested, &existing) {
        Ok(name) => name,
        Err(code) => return code,
    };
    ensure_local_unseeded_server(target, false);
    if let Err(code) = rt.block_on(create_empty_session_via_metadata(target, &name, false)) {
        return code;
    }
    let dial = target.dial();
    let predict_cfg = super::super::attach::predictive_config_for(&dial);
    match rt.block_on(run_attach_once(
        &dial,
        AttachTarget::ByName(name.clone()),
        predict_cfg,
    )) {
        Ok(attach_end) => {
            report_attach_end(attach_end);
            ExitCode::SUCCESS
        }
        Err(err) => {
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
        Ok(()) => crate::output::json(&empty_session_json(name)),
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

/// Create an empty, persistent session. Refuses before writing when the
/// server can create no empty session ([`phux_client::session::keep_empty_supported`]:
/// an older one would seed a shell).
pub(crate) async fn create_empty_session_via_metadata(
    server: &ServerTarget,
    name: &str,
    json: bool,
) -> Result<(), ExitCode> {
    let mut conn = server
        .connect()
        .await
        .map_err(|err| server.report_unreachable(json, &err, "new"))?;
    // The capability check runs before the duplicate-name `GET_STATE`, so an
    // unsupporting server costs exactly one round trip.
    if !phux_client::session::keep_empty_supported(&conn) {
        return Err(report_empty_unsupported(json));
    }
    reject_duplicate_session_name(&mut conn, server, name, json).await?;
    let mut notices = Vec::new();
    let result = phux_client::session::create_empty_session(&mut conn, name, &mut notices).await;
    drop(conn);
    // Printed before the error is reported, in encounter order: a notice
    // collected before a later transport failure must not be dropped.
    warn_partial_results(&notices);
    let outcome = result.map_err(|err| report_create_session_error(server, json, err))?;
    match outcome {
        CreateEmptyOutcome::Created => Ok(()),
        CreateEmptyOutcome::Unsupported => Err(report_empty_unsupported(json)),
        CreateEmptyOutcome::ReadRefused(refusal) => Err(report_create_failed(
            json,
            format!("server refused the read-back: {refusal}"),
        )),
        CreateEmptyOutcome::NotRegistered => Err(report_session_not_registered(json, name)),
        CreateEmptyOutcome::Refused(refusal) => Err(report_create_refused(json, name, &refusal)),
    }
}

/// The server cannot create a session with no terminal (ADR-0105).
pub(super) fn report_empty_unsupported(json: bool) -> ExitCode {
    use crate::commands::json_err::{CliError, codes, emit};
    emit(
        json,
        &CliError::new(
            codes::UNSUPPORTED_SERVER,
            "create-session failed: the server does not support empty sessions",
            "upgrade the server (`phux upgrade`), or create the session without --empty",
        ),
        1,
    )
}
