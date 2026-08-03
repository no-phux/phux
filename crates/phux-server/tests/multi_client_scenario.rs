//! `phux-1vl` (salvaged from `phux-a87`) — multi-client scenario.
//!
//! End-to-end test for the fanout half of SPEC §12 / ADR-0006: two
//! clients attached concurrently to the same session MUST both observe
//! the same `TERMINAL_OUTPUT` stream after keystrokes from either side.
//!
//! Coverage:
//!   * Both clients receive `ATTACHED + TERMINAL_SNAPSHOT` for the same
//!     pre-seeded session and the same `terminal_id`.
//!   * Allocated `ClientId`s are distinct (no slot reuse, per
//!     `ServerState::new_client_id`).
//!   * A keystroke from client A is observed (via a Screen render) on
//!     **both** A's and B's stream — proving the per-pane broadcast
//!     fanout reaches every subscriber, not just the originator.
//!
//! This complements `byc_6_3` (single-client detach) and
//! `reconnect_scenario` (serial reconnect): here we hold two streams
//! open simultaneously and assert the per-pane subscriber list contains
//! both. If fanout were buggy (e.g. broadcast capacity dropping older
//! receivers), one of the two assertions would fail.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]
#![allow(
    clippy::similar_names,
    reason = "client_a / client_b / screen_a / screen_b are the test's vocabulary"
)]

use phux_protocol::TerminalId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    FrameKind, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN, TYPE_TERMINAL_OUTPUT,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::screen::Screen;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

fn ascii_key(c: char, key: PhysicalKey) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: Some(c.to_string()),
        unshifted_codepoint: Some(c as u32),
    }
}

const fn enter_key() -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::Enter,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

/// Drain `TERMINAL_OUTPUT` from `stream` into `screen` until `needle`
/// appears or `WIRE_RECV_TIMEOUT` elapses. Returns total bytes consumed
/// for diagnostic reporting.
async fn drain_into_screen(stream: &mut UnixStream, screen: &mut Screen, needle: &str) -> usize {
    let mut total = 0usize;
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if type_byte != TYPE_TERMINAL_OUTPUT {
            continue;
        }
        if let FrameKind::TerminalOutput { bytes, .. } = frame {
            total += bytes.len();
            screen.write(&bytes);
            if screen.contains(needle) {
                return total;
            }
        }
    }
    total
}

/// Attach a fresh socket to `default` and drain the
/// `ATTACHED + TERMINAL_SNAPSHOT` opening sequence. Returns the open
/// stream, the allocated `ClientId`, and the snapshot's `terminal_id`.
async fn attach_default(socket_path: &std::path::Path) -> (UnixStream, u32, TerminalId) {
    let mut stream = wait_for_socket(socket_path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(&mut stream, &attach_by_name("default")).await;

    let (type_byte, attached) = recv_typed(&mut stream).await;
    assert_eq!(type_byte, TYPE_ATTACHED, "first frame must be ATTACHED");
    let (client_id, terminal_id) = match attached {
        FrameKind::Attached {
            attach_id: _,
            snapshot,
            initial_client_id,
        } => {
            assert_eq!(snapshot.panes.len(), 1, "exactly one pane");
            (initial_client_id.get(), snapshot.panes[0].id.clone())
        }
        other => panic!("expected Attached, got {other:?}"),
    };

    let (type_byte, _snap) = recv_typed(&mut stream).await;
    assert_eq!(
        type_byte, TYPE_BOOTSTRAP_BEGIN,
        "second frame must be TERMINAL_SNAPSHOT",
    );

    (stream, client_id, terminal_id)
}

#[test]
fn two_clients_attached_to_same_session_both_see_keystroke() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        let cmd = CommandBuilder::new("/bin/cat");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        // ============================================================
        // Phase 1 — both clients attach. Distinct ClientIds, same pane.
        // ============================================================
        let (mut client_a, client_a_id, terminal_id_a) = attach_default(&socket_path).await;
        let (mut client_b, client_b_id, terminal_id_b) = attach_default(&socket_path).await;

        assert_ne!(
            client_a_id, client_b_id,
            "ClientIds must be distinct (A={client_a_id}, B={client_b_id})",
        );
        assert_eq!(
            terminal_id_a, terminal_id_b,
            "Both clients must see the same shared terminal_id \
             (A={terminal_id_a:?}, B={terminal_id_b:?})",
        );

        // ============================================================
        // Phase 2 — client A sends a keystroke. Fanout invariant: the
        // TERMINAL_OUTPUT (from cat's echo) MUST reach BOTH streams.
        // ============================================================
        send_frame(
            &mut client_a,
            &FrameKind::InputKey {
                terminal_id: terminal_id_a.clone(),
                event: ascii_key('m', PhysicalKey::M),
            },
        )
        .await;
        send_frame(
            &mut client_a,
            &FrameKind::InputKey {
                terminal_id: terminal_id_a.clone(),
                event: enter_key(),
            },
        )
        .await;

        // Drain A and B's streams concurrently. If one client blocks
        // and the broadcast had a tiny capacity, the slower receiver
        // would see lag rather than the byte. Concurrent drain catches
        // that asymmetry — both Screens must converge to "m" visible.
        let mut screen_a = Screen::new(80, 24).expect("Screen::new A");
        let mut screen_b = Screen::new(80, 24).expect("Screen::new B");
        let (bytes_a, bytes_b) = tokio::join!(
            drain_into_screen(&mut client_a, &mut screen_a, "m"),
            drain_into_screen(&mut client_b, &mut screen_b, "m"),
        );

        assert!(
            screen_a.contains("m"),
            "client A: must observe its own echo. bytes={bytes_a}, screen=\n{}",
            screen_a.snapshot_text(),
        );
        assert!(
            screen_b.contains("m"),
            "client B: must observe A's keystroke via fanout. bytes={bytes_b}, screen=\n{}",
            screen_b.snapshot_text(),
        );

        // ============================================================
        // Teardown — drop both streams, shut server down, assert no
        // leaked socket FD.
        // ============================================================
        drop(client_a);
        drop(client_b);
        shutdown_tx.send(()).ok();
        timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server didn't shut down")
            .expect("server join")
            .expect("server run_async ok");

        assert!(
            !socket_path.exists(),
            "socket file leaked after shutdown: {} still on disk",
            socket_path.display(),
        );

        // Linux precise FD-leak guard. See `reconnect_scenario.rs` for
        // rationale; band check, not exact equality.
        #[cfg(target_os = "linux")]
        {
            let open = std::fs::read_dir("/proc/self/fd")
                .expect("read /proc/self/fd")
                .filter_map(Result::ok)
                .count();
            assert!(
                open < 256,
                "FD leak suspected: {open} open fds after teardown",
            );
        }
    });
}
