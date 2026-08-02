//! `phux-byc.6.3` — `detach_clean_shutdown`.
//!
//! Wire-level integration test for the DETACH path. Drives a real
//! `ServerRuntime` over a Unix-domain socket, exercises the full
//! attach -> detach lifecycle, and verifies that the server cleans
//! up per-client state cleanly enough that a *fresh* client can
//! re-attach to the same session without any stale residue.
//!
//! Coverage matrix (mapped to SPEC §7.3 and the byc.6.3 ticket):
//!
//! 1. After ATTACH the server emits ATTACHED + `TERMINAL_SNAPSHOT` as usual.
//! 2. Client sends DETACH; server replies with DETACHED.
//! 3. Client drops the stream; server observes EOF cleanly (no
//!    `client task ended with error` log line — proven indirectly by
//!    the next step succeeding).
//! 4. A *fresh* client connects, sends ATTACH, and gets a NEW
//!    ATTACHED whose `initial_client_id` is strictly greater than
//!    the first attach's id. Monotonic `ClientId` allocation
//!    (`ServerState::new_client_id`) means a re-issued id would
//!    indicate the registry was clobbered — distinct ids prove
//!    fresh-client cleanly attached to a still-intact session.
//! 5. The re-attaching client also gets a `TERMINAL_SNAPSHOT` — the
//!    pane actor survived the detach and is still serving snapshots.
//!
//! The byc.6.3 ticket asks us to verify the wire-level mirror of the
//! `ServerState::detach_removes_client_and_drops_empty_subscriber_lists`
//! unit test. We do not (and cannot) reach into `ServerState` from an
//! integration binary; instead we exercise the same invariant
//! externally: if `detach` failed to drop the outbound mailbox or the
//! `pane_subscribers` entry, the second attach would either hang
//! waiting for a slot or see corrupted snapshot state. Neither
//! happens here.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(
    clippy::similar_names,
    reason = "client_a_id vs client_b_id is the whole point of the test"
)]

mod common;

use phux_protocol::wire::frame::{FrameKind, TYPE_ATTACHED, TYPE_DETACHED, TYPE_TERMINAL_SNAPSHOT};
use tempfile::TempDir;

use crate::common::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame, spawn_server,
    wait_for_socket,
};

#[test]
fn byc_6_3_detach_releases_state_and_allows_fresh_reattach() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("default"));

        // ============================================================
        // Phase 1: Client A connects, attaches, captures initial_client_id.
        // ============================================================
        let mut client_a = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut client_a, &attach_by_name("default")).await;

        // Drain ATTACHED.
        let (type_byte, attached_a) = recv_typed(&mut client_a).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "client A: first frame ATTACHED");
        let client_a_id = match attached_a {
            FrameKind::Attached {
                attach_id: _,
                snapshot,
                initial_client_id,
            } => {
                assert_eq!(snapshot.sessions.len(), 1, "client A: one session");
                assert_eq!(snapshot.sessions[0].name, "default");
                initial_client_id.get()
            }
            other => panic!("client A: expected Attached, got {other:?}"),
        };
        assert!(
            client_a_id >= 1,
            "client A: initial_client_id must be allocated, got {client_a_id}",
        );

        // Drain TERMINAL_SNAPSHOT — required by SPEC §13 step 2.
        let (type_byte, _snap_a) = recv_typed(&mut client_a).await;
        assert_eq!(
            type_byte, TYPE_TERMINAL_SNAPSHOT,
            "client A: second frame TERMINAL_SNAPSHOT",
        );

        // ============================================================
        // Phase 2: Client A sends DETACH; server replies with DETACHED.
        // ============================================================
        send_frame(&mut client_a, &FrameKind::Detach).await;
        let (type_byte, detached) = recv_typed(&mut client_a).await;
        assert_eq!(
            type_byte, TYPE_DETACHED,
            "client A: server must reply DETACHED",
        );
        assert!(
            matches!(detached, FrameKind::Detached),
            "client A: payload must decode as Detached (got {detached:?})",
        );

        // ============================================================
        // Phase 3: Drop client A's socket. The server's read loop
        // observes EOF and runs the implicit-detach path in
        // `runtime::accept_loop`. We don't have a direct wire-level
        // signal for that — but the next phase's re-attach succeeding
        // is proof that the cleanup landed (or there'd be a leftover
        // subscriber entry / mailbox).
        // ============================================================
        drop(client_a);

        // ============================================================
        // Phase 4: Fresh client B connects + attaches. Must succeed,
        // must receive a NEW client id, must receive a fresh
        // TERMINAL_SNAPSHOT (proving the pane actor is still alive).
        // ============================================================
        let mut client_b = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut client_b, &attach_by_name("default")).await;

        let (type_byte, attached_b) = recv_typed(&mut client_b).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "client B: first frame ATTACHED");
        let client_b_id = match attached_b {
            FrameKind::Attached {
                attach_id: _,
                snapshot,
                initial_client_id,
            } => {
                // Session graph is intact across the detach: same
                // session, same window count, same pane count.
                assert_eq!(snapshot.sessions.len(), 1, "client B: one session");
                assert_eq!(snapshot.sessions[0].name, "default");
                assert_eq!(snapshot.windows.len(), 1, "client B: one window");
                assert_eq!(snapshot.panes.len(), 1, "client B: one pane");
                initial_client_id.get()
            }
            other => panic!("client B: expected Attached, got {other:?}"),
        };

        // Monotonic ClientId allocation. If B got the same id as A,
        // either A's id wasn't freed (and we'd have a leaked mailbox)
        // or the allocator regressed. Either is a bug.
        assert!(
            client_b_id > client_a_id,
            "client B id must be strictly greater than client A id \
             (a/b = {client_a_id}/{client_b_id}); equal ids would mean detach \
             didn't drop the slot",
        );

        // The pane actor survived: client B got a usable snapshot.
        let (type_byte, snap_b) = recv_typed(&mut client_b).await;
        assert_eq!(
            type_byte, TYPE_TERMINAL_SNAPSHOT,
            "client B: second frame TERMINAL_SNAPSHOT (pane actor still alive)",
        );
        match snap_b {
            FrameKind::TerminalSnapshot {
                cols,
                rows,
                vt_replay_bytes,
                scrollback_bytes,
                ..
            } => {
                assert_eq!(cols, 80, "client B: snapshot cols");
                assert_eq!(rows, 24, "client B: snapshot rows");
                assert!(
                    vt_replay_bytes.starts_with(b"\x1b[!p\x1b[2J\x1b[H"),
                    "client B: snapshot must carry the reset preamble",
                );
                assert!(
                    scrollback_bytes.is_none(),
                    "client B: byc.8 never emits scrollback_bytes",
                );
            }
            other => panic!("client B: expected TerminalSnapshot, got {other:?}"),
        }

        // Clean teardown.
        drop(client_b);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}
