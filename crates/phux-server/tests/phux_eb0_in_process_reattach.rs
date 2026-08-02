//! `phux-eb0` — in-process session re-attach over one connection.
//!
//! Wire-level proof of the contract the client's outer re-attach loop
//! depends on (`phux-client::attach::driver::run_with_stdout_predict`):
//! a single client connection can ATTACH to session A, DETACH, and then
//! ATTACH to a DIFFERENT session B **on the same transport connection**,
//! receiving B's ATTACHED snapshot + a fresh `TERMINAL_SNAPSHOT` for B's
//! seed pane.
//!
//! The client side tears down all session-scoped state and re-runs the
//! handshake between the DETACH and the second ATTACH; this test pins the
//! server-side half: that DETACH frees the per-consumer state without
//! closing the connection (so the same socket is reusable) and that the
//! re-ATTACH resolves a different session and snapshots its panes. If the
//! server closed the connection on DETACH, or refused a second ATTACH on
//! a connection that had already attached, the client's flicker-free
//! in-process switch would be impossible and this test would fail.
//!
//! Distinguishing A from B: the two sessions have distinct names and
//! distinct seed-pane `TerminalId`s, so the second ATTACHED's snapshot
//! (focused session name + focused pane id) must differ from the first.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

mod common;

use std::time::Duration;

use phux_protocol::wire::frame::{
    AttachTarget, FrameKind, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN, TYPE_DETACHED,
    TYPE_TERMINAL_OUTPUT, ViewportInfo,
};
use tempfile::TempDir;
use tokio::time::timeout;

use crate::common::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame, spawn_server,
    spawn_server_seed_pty_no_cmd, wait_for_socket,
};

/// `ATTACH { CreateIfMissing { name } }` at the canonical 80x24 viewport.
/// Used to materialize a second session over the wire without a second
/// pre-seed.
fn create_if_missing(name: &str) -> FrameKind {
    create_if_missing_with_command(name, None)
}

fn create_if_missing_with_command(name: &str, command: Option<Vec<String>>) -> FrameKind {
    FrameKind::Attach {
        attach_id: 1,
        target: AttachTarget::CreateIfMissing {
            name: name.to_owned(),
            command,
            cwd: None,
        },
        viewport: ViewportInfo::new(80, 24),
        request_scrollback: false,
        scrollback_limit_lines: 0,
    }
}

#[test]
fn reattach_to_other_session_on_same_connection_renders_b() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // Pre-seed session A ("alpha"); B ("beta") is created over the
        // wire by a throwaway connection so two sessions coexist.
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some("alpha"));

        // ------------------------------------------------------------
        // Materialize session B via a separate connection, then drop it.
        // The session survives the connection close (one server per user;
        // sessions are server-owned, not connection-owned).
        // ------------------------------------------------------------
        {
            let mut seed = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
            send_frame(&mut seed, &create_if_missing("beta")).await;
            let (type_byte, _attached) = recv_typed(&mut seed).await;
            assert_eq!(type_byte, TYPE_ATTACHED, "seed: ATTACHED for beta");
            let (type_byte, _snap) = recv_typed(&mut seed).await;
            assert_eq!(type_byte, TYPE_BOOTSTRAP_BEGIN, "seed: snapshot for beta");
            // Detach cleanly so the server frees the seed connection's
            // consumer state before we drop the socket.
            send_frame(&mut seed, &FrameKind::Detach).await;
            let (type_byte, _detached) = recv_typed(&mut seed).await;
            assert_eq!(type_byte, TYPE_DETACHED, "seed: DETACHED");
            drop(seed);
        }

        // ------------------------------------------------------------
        // The client connection: ATTACH to A.
        // ------------------------------------------------------------
        let mut client = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut client, &attach_by_name("alpha")).await;

        let (type_byte, attached_a) = recv_typed(&mut client).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "A: ATTACHED");
        let (a_focused_name, a_focused_pane) = match attached_a {
            FrameKind::Attached { snapshot, .. } => {
                // The snapshot lists both sessions (the server graph is
                // global), but the FOCUSED session is the one we attached
                // to: alpha.
                let focused = snapshot
                    .sessions
                    .iter()
                    .find(|s| s.id == snapshot.focused_session)
                    .expect("focused session in graph");
                assert_eq!(focused.name, "alpha", "A: focused session is alpha");
                (focused.name.clone(), snapshot.focused_pane.clone())
            }
            other => panic!("A: expected Attached, got {other:?}"),
        };
        // Drain A's seed-pane snapshot.
        let (type_byte, _snap_a) = recv_typed(&mut client).await;
        assert_eq!(type_byte, TYPE_BOOTSTRAP_BEGIN, "A: snapshot");

        // ------------------------------------------------------------
        // In-process switch: DETACH from A, then ATTACH to B on the SAME
        // connection. This is exactly what the client's outer loop does
        // when the user picks "beta" from the `<leader> a` picker.
        // ------------------------------------------------------------
        send_frame(&mut client, &FrameKind::Detach).await;
        let (type_byte, detached) = recv_typed(&mut client).await;
        assert_eq!(type_byte, TYPE_DETACHED, "switch: server replies DETACHED");
        assert!(
            matches!(detached, FrameKind::Detached),
            "switch: payload is Detached",
        );

        // Same connection, new target.
        send_frame(&mut client, &attach_by_name("beta")).await;
        let (type_byte, attached_b) = recv_typed(&mut client).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "B: re-ATTACH on the same connection must succeed",
        );
        let (b_focused_name, b_focused_pane) = match attached_b {
            FrameKind::Attached { snapshot, .. } => {
                let focused = snapshot
                    .sessions
                    .iter()
                    .find(|s| s.id == snapshot.focused_session)
                    .expect("focused session in graph");
                assert_eq!(focused.name, "beta", "B: focused session is now beta");
                (focused.name.clone(), snapshot.focused_pane.clone())
            }
            other => panic!("B: expected Attached, got {other:?}"),
        };

        // The switch actually re-targeted: focused session and seed pane
        // both differ from A. A stale mirror from A would carry A's pane
        // id; the client rebuilds all session-scoped state from THIS
        // snapshot, keyed on B's pane.
        assert_ne!(a_focused_name, b_focused_name, "session name changed A->B");
        assert_ne!(
            a_focused_pane, b_focused_pane,
            "focused pane differs across the switch (fresh session-scoped state)",
        );

        // The pane actor for B is alive and serving: B's TERMINAL_SNAPSHOT
        // arrives, proving the re-attach reconstructs B's terminal (the
        // client repaints from this).
        let (type_byte, begin_b) = recv_typed(&mut client).await;
        assert_eq!(type_byte, TYPE_BOOTSTRAP_BEGIN);
        match begin_b {
            FrameKind::BootstrapBegin {
                terminal_id,
                cols: 80,
                rows: 24,
                ..
            } => assert_eq!(terminal_id, b_focused_pane),
            other => panic!("B: expected BootstrapBegin, got {other:?}"),
        }
        let (_, chunk_b) = recv_typed(&mut client).await;
        assert!(matches!(
            chunk_b,
            FrameKind::BootstrapChunk { ref payload, .. }
                if payload.starts_with(b"\x1b[!p\x1b[2J\x1b[H")
        ));
        let (_, ready_b) = recv_typed(&mut client).await;
        assert!(matches!(ready_b, FrameKind::BootstrapReady { .. }));

        drop(client);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}

#[test]
fn reattach_to_other_session_does_not_forward_old_session_output() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server_seed_pty_no_cmd(socket_path.clone(), None);

        let alpha_cmd = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "i=0; while :; do i=$((i+1)); printf 'ALPHA-%d\\n' \"$i\"; sleep 0.05; done".to_owned(),
        ];
        let beta_cmd = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "i=0; while :; do i=$((i+1)); printf 'BETA-%d\\n' \"$i\"; sleep 0.05; done".to_owned(),
        ];

        // Materialize both sessions with deterministic, distinct output streams.
        for (name, command) in [("alpha", alpha_cmd), ("beta", beta_cmd)] {
            let mut seed = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
            send_frame(
                &mut seed,
                &create_if_missing_with_command(name, Some(command)),
            )
            .await;
            let (type_byte, _attached) = recv_typed(&mut seed).await;
            assert_eq!(type_byte, TYPE_ATTACHED, "seed {name}: ATTACHED");
            let (type_byte, _snap) = recv_typed(&mut seed).await;
            assert_eq!(type_byte, TYPE_BOOTSTRAP_BEGIN, "seed {name}: snapshot");
            send_frame(&mut seed, &FrameKind::Detach).await;
            loop {
                let (type_byte, frame) = recv_typed(&mut seed).await;
                if type_byte == TYPE_TERMINAL_OUTPUT {
                    continue;
                }
                assert_eq!(type_byte, TYPE_DETACHED, "seed {name}: DETACHED");
                assert!(matches!(frame, FrameKind::Detached));
                break;
            }
        }

        let mut client = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut client, &attach_by_name("alpha")).await;
        let (type_byte, _attached_a) = recv_typed(&mut client).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "A: ATTACHED");
        let (type_byte, _snap_a) = recv_typed(&mut client).await;
        assert_eq!(type_byte, TYPE_BOOTSTRAP_BEGIN, "A: snapshot");

        // Prove the alpha raw-output pump is live before the switch.
        let mut saw_alpha = false;
        for _ in 0..20 {
            let (type_byte, frame) = recv_typed(&mut client).await;
            if type_byte == TYPE_TERMINAL_OUTPUT
                && let FrameKind::TerminalOutput { bytes, .. } = frame
                && bytes.windows(b"ALPHA".len()).any(|w| w == b"ALPHA")
            {
                saw_alpha = true;
                break;
            }
        }
        assert!(saw_alpha, "A: expected live ALPHA output before detach");

        send_frame(&mut client, &FrameKind::Detach).await;
        loop {
            let (type_byte, frame) = recv_typed(&mut client).await;
            if type_byte == TYPE_TERMINAL_OUTPUT {
                continue;
            }
            assert_eq!(type_byte, TYPE_DETACHED, "switch: DETACHED");
            assert!(matches!(frame, FrameKind::Detached));
            break;
        }

        send_frame(&mut client, &attach_by_name("beta")).await;
        let (type_byte, _attached_b) = recv_typed(&mut client).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "B: ATTACHED");
        let (type_byte, _snap_b) = recv_typed(&mut client).await;
        assert_eq!(type_byte, TYPE_BOOTSTRAP_BEGIN, "B: snapshot");

        // After B attaches on the same connection, only B's pump should feed this mailbox.
        // Before phux-lskb, A's orphaned pump could still forward ALPHA output here.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(700);
        let mut saw_beta = false;
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline - tokio::time::Instant::now();
            let Ok((type_byte, frame)) = timeout(remaining, recv_typed(&mut client)).await else {
                break;
            };
            if type_byte != TYPE_TERMINAL_OUTPUT {
                continue;
            }
            let FrameKind::TerminalOutput { bytes, .. } = frame else {
                continue;
            };
            assert!(
                !bytes.windows(b"ALPHA".len()).any(|w| w == b"ALPHA"),
                "old alpha output pump forwarded after beta attach: {:?}",
                String::from_utf8_lossy(&bytes),
            );
            if bytes.windows(b"BETA".len()).any(|w| w == b"BETA") {
                saw_beta = true;
            }
        }
        assert!(saw_beta, "B: expected live BETA output after reattach");

        drop(client);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}
