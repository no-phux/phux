use std::process::ExitCode;

use phux_client::session::RenameError;

use crate::commands::ctx;
use crate::commands::json_err::Refusal;
use crate::commands::server_target::{ServerSpec, ServerTarget};
use crate::exit_codes::EXIT_USAGE;

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
    ctx::run(server, "rename", json, |ctx| {
        ctx.rt
            .block_on(rename_session(&ctx.target, target, new_name, json))
    })
}

/// Version of the session `rename --json` document.
pub(crate) const RENAME_SCHEMA_VERSION: u8 = 1;

/// Rename the session `session` on `target`, reporting a refusal itself;
/// see [`run_rename`].
pub(crate) async fn run_rename_on(
    target: &ServerTarget,
    session: &str,
    new_name: &str,
    json: bool,
) -> ExitCode {
    rename_session(target, session, new_name, json)
        .await
        .unwrap_or_else(|refusal| refusal.emit(json))
}

async fn rename_session(
    target: &ServerTarget,
    session: &str,
    new_name: &str,
    json: bool,
) -> Result<ExitCode, Refusal> {
    let mut conn = target
        .connect()
        .await
        .map_err(|err| target.classify(&err, "rename"))?;

    // Session names are hub-local, so a partial fleet cannot hide the session
    // or a collision: warn and exit 0 (unlike `kill`/`tag`).
    let mut notices = Vec::new();
    let result =
        phux_client::session::rename_checked(&mut conn, session, new_name, &mut notices).await;
    for notice in &notices {
        warn!("{} saw only part of the fleet — {notice}", "rename");
    }
    result.map_err(|err| rename_refusal(target, session, err))?;
    conn.shutdown().await;
    if json {
        return Ok(crate::output::json(&serde_json::json!({
            "schema_version": RENAME_SCHEMA_VERSION,
            "session": new_name,
            "renamed_from": session,
        })));
    }
    outln!("renamed {session:?} to {new_name:?}");
    Ok(ExitCode::SUCCESS)
}

/// A rename that did not happen: unreachable server, exit 1, or the
/// server's refusal, exit 2.
fn rename_refusal(target: &ServerTarget, session: &str, err: RenameError) -> Refusal {
    if let RenameError::Attach(err) = err {
        return target.classify(&err, "rename");
    }
    Refusal::new(
        "server_error",
        format!("rename refused for session {session:?}: {}", err.reason()),
        "run `phux ls` to see session names; a NEW_NAME already in use is refused",
        EXIT_USAGE,
    )
}

/// Run `phux rename` as parsed.
pub(crate) fn run(
    args: crate::commands::RenameArgs,
    socket: Option<std::path::PathBuf>,
) -> ExitCode {
    let crate::commands::RenameArgs {
        target,
        new_name,
        json,
        remote,
    } = args;
    run_rename(&target, &new_name, json.json, remote.with_socket(socket))
}
