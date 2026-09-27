use std::process::ExitCode;

use crate::commands::server_target::ServerSpec;

/// `phux rename SESSION NEW_NAME` — an L3 `SET_METADATA` of
/// `SESSION_NAME_KEY` via [`phux_client::session::rename_checked`], which the
/// server intercepts and applies. `SET_METADATA` has no reply, so existence and
/// collisions are checked against a fresh `GET_STATE` first (a no-op sends
/// nothing), and a second `GET_STATE` is the ordering barrier that confirms the
/// rename. Exit 0 success, 1 no server, 2 refusal.
#[expect(
    clippy::significant_drop_tightening,
    reason = "shutdown consumes the connection after the final ordering barrier"
)]
pub(crate) fn run_rename(session: &str, new_name: &str, server: ServerSpec) -> ExitCode {
    let (rt, target) = match server.prepare("rename", false) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };

    rt.block_on(async move {
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
    })
}
