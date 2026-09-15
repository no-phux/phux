//! `phux-client` wire primitives for session-identity writes.
//!
//! Today this covers `phux rename`; create-without-attach (`phux new`,
//! `phux new --json`) still hand-rolls its `SESSION_CREATE_KEY` write in
//! `crates/phux/src/commands/new.rs` pending a follow-up migration into this
//! module (ADR-0022 §5 names the intended shape).

use phux_protocol::wire::frame::{FrameKind, SESSION_NAME_KEY, Scope};

use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// The conventional rename write: `current\0new` under [`SESSION_NAME_KEY`].
///
/// Since the v0.3.0 "Option B" re-tier (ADR-0019 / ADR-0027) dissolved the
/// L2 collection tier and removed the `RENAME_SESSION` verb, a rename is
/// expressed as an L3 `SET_METADATA` write of this conventional key
/// (`Scope::Global`, value `current\0new`). The server is authoritative —
/// it intercepts this write and applies the registry rename.
#[must_use]
pub fn rename_frame(request_id: u32, session: &str, new_name: &str) -> FrameKind {
    let mut value = session.as_bytes().to_vec();
    value.push(0);
    value.extend_from_slice(new_name.as_bytes());
    FrameKind::SetMetadata {
        request_id,
        scope: Scope::Global,
        key: SESSION_NAME_KEY.to_owned(),
        value,
    }
}

/// Send the fire-and-forget rename write.
///
/// `SET_METADATA` carries no reply frame, so existence and name-collision
/// checks are the caller's job against a fresh `GET_STATE` snapshot, before
/// and (as an ordering barrier) after this call.
///
/// `request_id` is the caller's to allocate: earlier revisions of this path
/// hardcoded `1` inside the write itself, so a caller composing two renames
/// on one connection sent the identical id twice. Taking it as a parameter
/// lets a caller vary it — see the `two_renames_on_one_connection_correlate`
/// test.
///
/// # Errors
///
/// Transport failures from [`Connection::send`].
pub async fn rename(
    conn: &mut Connection,
    request_id: u32,
    session: &str,
    new_name: &str,
) -> Result<(), AttachError> {
    conn.send(&rename_frame(request_id, session, new_name))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rename_write_is_current_nul_new_under_the_conventional_key() {
        assert_eq!(
            rename_frame(7, "work", "play"),
            FrameKind::SetMetadata {
                request_id: 7,
                scope: Scope::Global,
                key: SESSION_NAME_KEY.to_owned(),
                value: b"work\0play".to_vec(),
            }
        );
    }

    #[tokio::test]
    async fn two_renames_on_one_connection_correlate() {
        // Historically both writes hardcoded request_id 1; a caller
        // allocating distinct ids per call (the fix) is pinned here against
        // a scripted server that pins the two frames it sees.
        let temp = tempfile::TempDir::new().expect("tempdir");
        let socket = temp.path().join("rename.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind");
        let spec = crate::testkit::ScriptSpec::new();
        let server_task =
            tokio::spawn(
                async move { crate::testkit::ScriptedServer::accept(&listener, spec).await },
            );

        let mut conn = Connection::connect(&socket).await.expect("connect");
        rename(&mut conn, 1, "a", "b")
            .await
            .expect("first rename send");
        rename(&mut conn, 2, "a", "c")
            .await
            .expect("second rename send");
        drop(conn);
        let seen = server_task.await.expect("scripted server");
        let ids: Vec<u32> = seen
            .into_iter()
            .filter_map(|frame| match frame {
                FrameKind::SetMetadata {
                    request_id, key, ..
                } if key == SESSION_NAME_KEY => Some(request_id),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec![1, 2], "each rename must carry its own request id");
    }
}
