use std::process::ExitCode;

use crate::commands::server_target::ServerSpec;

/// `phux rename SESSION NEW_NAME` — reassign a session's name.
///
/// Since the v0.3.0 "Option B" re-tier (ADR-0019 / ADR-0027) dissolved the
/// L2 collection tier and removed the `RENAME_SESSION` verb, a rename is now
/// expressed as an L3 `SET_METADATA` write of the conventional
/// `SESSION_NAME_KEY` (`Scope::Global`, value `current\0new`) built by
/// [`phux_client::session::rename_checked`]. The server is authoritative — it
/// intercepts that write and applies the registry rename, so attached
/// clients reconcile the new name on their next snapshot.
///
/// `SET_METADATA` is fire-and-forget (no reply frame), so existence and
/// name-collision checks are done client-side against a fresh `GET_STATE`
/// snapshot before the write. A no-op (the session already has `new_name`)
/// sends nothing. A second `GET_STATE` after a real write is the ordering
/// barrier and the outcome: frames are ordered on one connection, so once
/// the server answers it has processed the write, and the snapshot must
/// show the new name. Exit codes mirror `phux kill`: 0 on success, 1 on
/// no server, 2 on a refusal (unknown session, a name already taken, or a
/// barrier that did not apply the rename).
///
/// `server` is the local socket or a `--remote` host (see `server_target`).
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

        // Both checks read `sessions`, and a rename is the one target-shaped
        // verb a partial fleet cannot mislead: `handle_get_state_federated`
        // discards every satellite's `sessions` and `windows` list outright
        // (their `u32` ids would collide with the hub's), so the session name
        // space is hub-local whether or not a satellite answered. An
        // unreachable satellite can neither hide the session being renamed
        // nor conceal a collision with the new name. Hence a warning and a
        // full exit 0 here, where `kill`/`tag` refuse — the difference is in
        // what each verb searches, not in how careful it is.
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
