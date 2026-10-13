use std::process::ExitCode;

use crate::commands::server_target::{ServerSpec, ServerTarget};

/// `phux rename TARGET NEW_NAME`. A pane selector labels that resource
/// ([`crate::commands::org::run_rename_resource`]). A session is renamed by
/// [`phux_client::session::rename_checked`]: one `SESSION_RENAME` against a
/// server that advertises `ORGANIZATION`, else the `SESSION_NAME_KEY` write,
/// checked against a fresh `GET_STATE` first (a no-op sends nothing) and
/// confirmed by a second. Exit 0 success, 1 no server, 2 refusal.
pub(crate) fn run_rename(target: &str, new_name: &str, server: ServerSpec) -> ExitCode {
    if crate::commands::org::names_resource(target) {
        return crate::commands::org::run_rename_resource(target, new_name, server);
    }
    let (rt, server) = match server.prepare("rename", false) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    rt.block_on(run_rename_on(&server, target, new_name))
}

/// Rename the session `session` on `target`; see [`run_rename`].
#[expect(
    clippy::significant_drop_tightening,
    reason = "shutdown consumes the connection after the final ordering barrier"
)]
pub(crate) async fn run_rename_on(
    target: &ServerTarget,
    session: &str,
    new_name: &str,
) -> ExitCode {
    let mut conn = match target.connect().await {
        Ok(conn) => conn,
        Err(err) => return target.report_unreachable(false, &err, "rename"),
    };

    // Session names are hub-local, so a partial fleet cannot hide the session
    // or a collision: warn and exit 0 (unlike `kill`/`tag`).
    let mut notices = Vec::new();
    let result =
        phux_client::session::rename_checked(&mut conn, session, new_name, &mut notices).await;
    for notice in &notices {
        eprintln!(
            "{}",
            phux_client::state::partial_view_warning("rename", notice)
        );
    }
    match result {
        Ok(()) => {
            conn.shutdown().await;
            outln!("renamed {session:?} to {new_name:?}");
            ExitCode::SUCCESS
        }
        Err(phux_client::session::RenameError::Attach(err)) => {
            target.report_unreachable(false, &err, "rename")
        }
        Err(err) => {
            eprintln!(
                "phux: rename refused for session {session:?}: {}",
                err.reason()
            );
            ExitCode::from(2)
        }
    }
}
