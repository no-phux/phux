use std::process::ExitCode;

use phux_protocol::wire::frame::{FrameKind, SESSION_NAME_KEY, Scope};
use phux_protocol::wire::info::SessionSnapshot;

use crate::commands::partial;
use crate::commands::server_target::ServerSpec;

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
/// snapshot before the write. A second `GET_STATE` after it is an ordering
/// barrier: frames are ordered on one connection, so once the server answers
/// it has processed the write, and a QUIC connection is never closed with the
/// write still in flight. Exit codes mirror `phux kill`: 0 on success, 1 on
/// no server, 2 on a refusal (unknown session or a name already taken).
///
/// `server` is the local socket or a `--remote` host (see `server_target`).
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

        // Validate against a fresh snapshot: the target must exist and the
        // new name must be free (the server enforces this too, but it has no
        // reply channel for SET_METADATA, so we surface the diagnostic here).
        //
        // Both checks read `sessions`, and a rename is the one target-shaped
        // verb a partial fleet cannot mislead: `handle_get_state_federated`
        // discards every satellite's `sessions` and `windows` list outright
        // (their `u32` ids would collide with the hub's), so the session name
        // space is hub-local whether or not a satellite answered. An
        // unreachable satellite can neither hide the session being renamed
        // nor conceal a collision with the new name. Hence a warning and a
        // full exit 0 here, where `kill`/`tag` refuse — the difference is in
        // what each verb searches, not in how careful it is.
        let (snapshot, degradation) = match phux_client::state::get_state_on(&mut conn).await {
            Ok(view) => view.into_parts(),
            Err(err) => return target.report_unreachable(false, &err, "rename"),
        };
        partial::warn_partial_view("rename", &degradation);

        if let Some(reason) = rename_refusal(&snapshot, session, new_name) {
            eprintln!("phux: rename refused for session {session:?}: {reason}");
            return ExitCode::from(2);
        }

        if let Err(err) = conn.send(&rename_frame(session, new_name)).await {
            return target.report_unreachable(false, &err, "rename");
        }

        // An ordering barrier, not a verdict: once the server answers this
        // GET_STATE it has processed the write before it. Without it a QUIC
        // connection could close (see `QuicWriter`'s Drop) with the write
        // still unsent, and the line below would claim a rename that never
        // reached the server. That argument rests on `QuicWriter`'s close
        // semantics; the loopback e2e does not reproduce the race, so no
        // test pins it.
        if let Err(err) = phux_client::state::get_state_on(&mut conn).await {
            return target.report_unreachable(false, &err, "rename");
        }
        conn.shutdown().await;

        outln!("renamed {session:?} to {new_name:?}");
        ExitCode::SUCCESS
    })
}

/// Why the rename must not be sent, judged against the pre-write snapshot:
/// an unknown session, or a new name another session already holds.
fn rename_refusal(snapshot: &SessionSnapshot, session: &str, new_name: &str) -> Option<String> {
    if !has_session(snapshot, session) {
        return Some("no such session".to_owned());
    }
    if session != new_name && has_session(snapshot, new_name) {
        return Some(format!("{new_name:?} already exists"));
    }
    None
}

/// Whether `snapshot` holds a session named `name`.
fn has_session(snapshot: &SessionSnapshot, name: &str) -> bool {
    snapshot.sessions.iter().any(|s| s.name == name)
}

/// The conventional rename write: `current\0new` under [`SESSION_NAME_KEY`].
fn rename_frame(session: &str, new_name: &str) -> FrameKind {
    let mut value = session.as_bytes().to_vec();
    value.push(0);
    value.extend_from_slice(new_name.as_bytes());
    FrameKind::SetMetadata {
        request_id: 1,
        scope: Scope::Global,
        key: SESSION_NAME_KEY.to_owned(),
        value,
    }
}

#[cfg(test)]
mod tests {
    use phux_protocol::wire::frame::{FrameKind, SESSION_NAME_KEY, Scope};
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
    use phux_protocol::{ResourceId, SessionId, WindowId};

    use super::{rename_frame, rename_refusal};

    fn snapshot(names: &[&str]) -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::new(1)).with_sessions(
            names
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    SessionInfo::new(SessionId::new(u32::try_from(i).unwrap_or(0)), *name)
                })
                .collect(),
        )
    }

    #[test]
    fn refuses_an_unknown_session_and_a_taken_name() {
        let snap = snapshot(&["work", "play"]);
        assert_eq!(
            rename_refusal(&snap, "gone", "x").as_deref(),
            Some("no such session")
        );
        assert_eq!(
            rename_refusal(&snap, "work", "play").as_deref(),
            Some("\"play\" already exists")
        );
        assert_eq!(rename_refusal(&snap, "work", "fresh"), None);
        // Renaming to the same name is not a collision with itself.
        assert_eq!(rename_refusal(&snap, "work", "work"), None);
    }

    #[test]
    fn the_rename_write_is_current_nul_new_under_the_conventional_key() {
        assert_eq!(
            rename_frame("work", "play"),
            FrameKind::SetMetadata {
                request_id: 1,
                scope: Scope::Global,
                key: SESSION_NAME_KEY.to_owned(),
                value: b"work\0play".to_vec(),
            }
        );
    }
}
