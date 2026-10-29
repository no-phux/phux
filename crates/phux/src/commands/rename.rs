use std::process::ExitCode;

use crate::commands::server_target::{ServerSpec, ServerTarget};

/// `phux rename TARGET NEW_NAME`. A pane selector labels that resource
/// ([`crate::commands::org::run_rename_resource`]). A session is renamed by
/// [`phux_client::session::rename_checked`]: one `SESSION_RENAME` against a
/// server that advertises `ORGANIZATION`, else the `SESSION_NAME_KEY` write,
/// checked against a fresh `GET_STATE` first (a no-op sends nothing) and
/// confirmed by a second. Exit 0 success, 1 no server, 2 refusal.
pub(crate) fn run_rename(target: &str, new_name: &str, json: bool, server: ServerSpec) -> ExitCode {
    if crate::commands::org::names_resource(target) {
        return crate::commands::org::run_rename_resource(target, new_name, json, server);
    }
    let (rt, server) = match server.prepare("rename", json) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    rt.block_on(run_rename_on(&server, target, new_name, json))
}

/// Version of the session `rename --json` document.
pub(crate) const RENAME_SCHEMA_VERSION: u8 = 1;

/// Rename the session `session` on `target`; see [`run_rename`].
pub(crate) async fn run_rename_on(
    target: &ServerTarget,
    session: &str,
    new_name: &str,
    json: bool,
) -> ExitCode {
    let mut conn = match target.connect().await {
        Ok(conn) => conn,
        Err(err) => return target.report_unreachable(json, &err, "rename"),
    };

    // Session names are hub-local, so a partial fleet cannot hide the session
    // or a collision: warn and exit 0 (unlike `kill`/`tag`).
    let mut notices = Vec::new();
    let result =
        phux_client::session::rename_checked(&mut conn, session, new_name, &mut notices).await;
    for notice in &notices {
        warn!("{} saw only part of the fleet — {notice}", "rename");
    }
    match result {
        Ok(()) => {
            conn.shutdown().await;
            if json {
                return crate::output::json(&serde_json::json!({
                    "schema_version": RENAME_SCHEMA_VERSION,
                    "session": new_name,
                    "renamed_from": session,
                }));
            }
            outln!("renamed {session:?} to {new_name:?}");
            ExitCode::SUCCESS
        }
        Err(phux_client::session::RenameError::Attach(err)) => {
            target.report_unreachable(json, &err, "rename")
        }
        Err(err) => crate::commands::json_err::emit(
            json,
            &crate::commands::json_err::CliError::new(
                "server_error",
                format!("rename refused for session {session:?}: {}", err.reason()),
                "run `phux ls` to see session names; a NEW_NAME already in use is refused",
            ),
            2,
        ),
    }
}
