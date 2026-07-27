use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::connection::Connection;
use phux_protocol::wire::frame::{FrameKind, SESSION_NAME_KEY, Scope};
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, report_no_server};

/// `phux rename SESSION NEW_NAME` — reassign a session's name.
///
/// Since the v0.3.0 "Option B" re-tier (ADR-0019 / ADR-0027) dissolved the
/// L2 collection tier and removed the `RENAME_SESSION` verb, a rename is now
/// expressed as an L3 `SET_METADATA` write of the conventional
/// [`SESSION_NAME_KEY`] (`Scope::Global`, value `current\0new`). The server
/// is authoritative — it intercepts that write and applies the registry
/// rename, so attached clients reconcile the new name on their next
/// snapshot.
///
/// `SET_METADATA` is fire-and-forget (no reply frame), so existence and
/// name-collision checks are done client-side against a fresh `GET_STATE`
/// snapshot before the write. Exit codes mirror `phux kill`: 0 on success,
/// 1 on no server, 2 on a refusal (unknown session or a name already taken).
pub(crate) fn run_rename(session: &str, new_name: &str, socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    rt.block_on(async move {
        let mut conn = match Connection::connect(&socket_path).await {
            Ok(conn) => conn,
            Err(err) => return report_no_server(&err, &socket_path, "rename"),
        };

        // Validate against a fresh snapshot: the target must exist and the
        // new name must be free (the server enforces this too, but it has no
        // reply channel for SET_METADATA, so we surface the diagnostic here).
        let snapshot = match phux_client::state::get_state_on(&mut conn).await {
            Ok(snapshot) => snapshot,
            Err(err) => return report_no_server(&err, &socket_path, "rename"),
        };

        let names: Vec<&str> = snapshot.sessions.iter().map(|s| s.name.as_str()).collect();
        if !names.contains(&session) {
            eprintln!("phux: rename refused for session {session:?}: no such session");
            return ExitCode::from(2);
        }
        if session != new_name && names.contains(&new_name) {
            eprintln!("phux: rename refused for session {session:?}: {new_name:?} already exists");
            return ExitCode::from(2);
        }

        // Encode the rename as `current\0new` and write the conventional key.
        let mut value = session.as_bytes().to_vec();
        value.push(0);
        value.extend_from_slice(new_name.as_bytes());
        if let Err(err) = conn
            .send(&FrameKind::SetMetadata {
                request_id: 1,
                scope: Scope::Global,
                key: SESSION_NAME_KEY.to_owned(),
                value,
            })
            .await
        {
            return report_no_server(&err, &socket_path, "rename");
        }

        outln!("renamed {session:?} to {new_name:?}");
        ExitCode::SUCCESS
    })
}
