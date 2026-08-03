//! `phux-1vl` (salvaged from `phux-a87`) — reconnect scenario.
//!
//! End-to-end test for the SPEC §7.3 / §13 detach-then-reattach lifecycle
//! from the user-visible perspective: a single client drives a real PTY
//! through input, observes echoed `TERMINAL_OUTPUT`, drops the stream
//! mid-conversation, then a fresh client re-attaches and sees a usable
//! protocol-0.7 bootstrap followed by resumed `TERMINAL_OUTPUT` once the PTY
//! produces more bytes.
//!
//! This test also absorbs the server-side cleanup assertions that used to
//! live in `byc_6_3_detach_clean_shutdown.rs` (monotonic `ClientId`
//! allocation across detach, reset preamble + no retained-history cursor on
//! the reattach bootstrap). On top of those it makes the renderer-level
//! assertion: after reconnect, the **rendered**
//! `Screen` observes the post-reconnect echo (proving the bootstrap+stream
//! actually replays into a working VT) — that is the bit a user would
//! notice if the pane actor were torn down or its subscribers list got
//! corrupted on detach.
//!
//! AC mapping (salvaged from the `phux-a87` epic):
//!   * Reconnect after detach with a fresh protocol-0.7 bootstrap.
//!   * Subsequent `TERMINAL_OUTPUT` lands on the new client.
//!   * Cross-platform smoke teardown: the socket file MUST be unlinked
//!     after the server shuts down (`TempDir` cleanup is independent).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]
#![allow(
    clippy::similar_names,
    reason = "client_a_id vs client_b_id is the whole point of the folded byc_6_3 assertion"
)]

mod common;

use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    FrameKind, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN, TYPE_DETACHED, TYPE_TERMINAL_OUTPUT,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use crate::common::screen::Screen;
use crate::common::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// One ASCII press, no modifiers. Mirrors `screen_harness_demo::ascii_key`.
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

/// Enter — cooked-mode `cat` is line-buffered, so this flushes the echo.
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

/// Drain `TERMINAL_OUTPUT` frames into `screen` until `needle` appears in
/// the rendered grid or `WIRE_RECV_TIMEOUT` elapses. Mirrors
/// `screen_harness_demo::drain_into_screen` — kept private here so the
/// reconnect test reads as a single self-contained scenario.
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

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "linear scenario test reads better as a single function"
)]
fn reconnect_after_detach_replays_snapshot_and_resumes_output() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // `cat` echoes stdin → stdout in cooked mode; deterministic fixture.
        let cmd = CommandBuilder::new("/bin/cat");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        // ============================================================
        // Phase 1 — client A attaches, drives 'a' + Enter through the
        // PTY, observes echo through a Screen. Then DETACHes cleanly.
        // ============================================================
        let mut client_a = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut client_a, &attach_by_name("default")).await;

        let (type_byte, attached_a) = recv_typed(&mut client_a).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "client A: first frame ATTACHED");
        let (terminal_id_a, client_a_id) = match attached_a {
            FrameKind::Attached {
                snapshot,
                initial_client_id,
                ..
            } => (snapshot.panes[0].id.clone(), initial_client_id.get()),
            other => panic!("client A: expected Attached, got {other:?}"),
        };
        // Folded from `byc_6_3_detach_clean_shutdown`: the id must be a
        // real server allocation (monotonic from 1).
        assert!(
            client_a_id >= 1,
            "client A: initial_client_id must be allocated, got {client_a_id}",
        );

        let (type_byte, _snap_a) = recv_typed(&mut client_a).await;
        assert_eq!(
            type_byte, TYPE_BOOTSTRAP_BEGIN,
            "client A: second frame BOOTSTRAP_BEGIN",
        );

        // Send 'a' + Enter, observe the echo through a Screen.
        send_frame(
            &mut client_a,
            &FrameKind::InputKey {
                terminal_id: terminal_id_a.clone(),
                event: ascii_key('a', PhysicalKey::A),
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

        let mut screen_a = Screen::new(80, 24).expect("Screen::new");
        let bytes_a = drain_into_screen(&mut client_a, &mut screen_a, "a").await;
        assert!(
            screen_a.contains("a"),
            "client A: 'a' must round-trip through the PTY before detach. \
             bytes={bytes_a}, screen=\n{}",
            screen_a.snapshot_text(),
        );

        // Clean DETACH so the server runs the explicit detach path
        // (vs. an EOF-only implicit detach, which is covered in
        // eof_detach.rs). In-flight TERMINAL_OUTPUT
        // from the pre-detach 'a' echo may still arrive before the
        // server processes the DETACH command — drain past them.
        send_frame(&mut client_a, &FrameKind::Detach).await;
        let detached_deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        loop {
            let remaining = detached_deadline - tokio::time::Instant::now();
            let (type_byte, frame) = timeout(remaining, recv_typed(&mut client_a))
                .await
                .expect("client A: DETACHED never arrived");
            if type_byte == TYPE_TERMINAL_OUTPUT {
                continue; // pre-detach echo still draining
            }
            assert_eq!(type_byte, TYPE_DETACHED, "client A: DETACHED reply");
            assert!(
                matches!(frame, FrameKind::Detached),
                "client A: DETACHED payload (got {frame:?})",
            );
            break;
        }
        drop(client_a);

        // ============================================================
        // Phase 2 — client B reconnects to the same session. MUST get:
        //   (a) fresh ATTACHED whose snapshot still shows the pane,
        //   (b) fresh bootstrap (proves the actor survived),
        //   (c) on new keystrokes, fresh TERMINAL_OUTPUT carrying the
        //       PTY echo (proves the subscriber rewire actually streams).
        // ============================================================
        let mut client_b = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut client_b, &attach_by_name("default")).await;

        let (type_byte, attached_b) = recv_typed(&mut client_b).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "client B: first frame ATTACHED");
        let (terminal_id_b, client_b_id) = match attached_b {
            FrameKind::Attached {
                snapshot,
                initial_client_id,
                ..
            } => {
                assert_eq!(snapshot.sessions.len(), 1, "client B: one session");
                assert_eq!(snapshot.sessions[0].name, "default");
                assert_eq!(snapshot.windows.len(), 1, "client B: one window");
                assert_eq!(snapshot.panes.len(), 1, "client B: one pane (actor alive)");
                (snapshot.panes[0].id.clone(), initial_client_id.get())
            }
            other => panic!("client B: expected Attached, got {other:?}"),
        };
        assert_eq!(
            terminal_id_a, terminal_id_b,
            "client B: terminal_id is stable across reconnect",
        );

        // Folded from `byc_6_3_detach_clean_shutdown`: monotonic ClientId
        // allocation (`ServerState::new_client_id`). If B got the same id
        // as A, either A's slot wasn't freed on detach (leaked mailbox /
        // subscriber entry) or the allocator regressed. Either is a bug.
        assert!(
            client_b_id > client_a_id,
            "client B id must be strictly greater than client A id \
             (a/b = {client_a_id}/{client_b_id}); equal ids would mean detach \
             didn't drop the slot",
        );

        // The fresh bootstrap is the resume-from-snapshot half of SPEC §13:
        // the client must be able to reconstruct the pre-reconnect grid
        // without any backfilled TERMINAL_OUTPUT.
        let (type_byte, begin_b) = recv_typed(&mut client_b).await;
        assert_eq!(type_byte, TYPE_BOOTSTRAP_BEGIN);
        let (snap_cols, snap_rows) = match begin_b {
            FrameKind::BootstrapBegin { cols, rows, .. } => (cols, rows),
            other => panic!("client B: expected BootstrapBegin, got {other:?}"),
        };
        let (_, chunk_b) = recv_typed(&mut client_b).await;
        let snap_bytes = match chunk_b {
            FrameKind::BootstrapChunk { payload, .. } => payload,
            other => panic!("client B: expected BootstrapChunk, got {other:?}"),
        };
        let history_cursor = match recv_typed(&mut client_b).await.1 {
            FrameKind::BootstrapReady { history_cursor, .. } => history_cursor,
            other => panic!("client B: expected BootstrapReady, got {other:?}"),
        };
        assert_eq!(snap_cols, 80, "client B: snapshot cols");
        assert_eq!(snap_rows, 24, "client B: snapshot rows");
        // Folded from `byc_6_3_detach_clean_shutdown`: the reattach
        // bootstrap must carry the reset preamble (never empty).
        assert!(
            snap_bytes.starts_with(b"\x1b[!p\x1b[2J\x1b[H"),
            "client B: bootstrap must carry the reset preamble",
        );
        assert!(
            history_cursor.is_none(),
            "client B: no history cursor when scrollback was not requested",
        );

        // Build a fresh Screen from the snapshot bytes — this is what a
        // reconnecting client renders before any live TERMINAL_OUTPUT
        // arrives. We do NOT assert the pre-detach 'a' echo is visible
        // in the snapshot replay (cat strips it once it scrolls / the
        // synthesiser may not emit pure echo lines) — instead we prove
        // the live stream resumes by sending a fresh keystroke.
        let mut screen_b = Screen::new(80, 24).expect("Screen::new");
        screen_b.write(&snap_bytes);

        // Drive a *new* keystroke through the reconnected client. The
        // TERMINAL_OUTPUT must reach screen_b, proving the post-reconnect
        // subscription is wired and the actor is alive.
        send_frame(
            &mut client_b,
            &FrameKind::InputKey {
                terminal_id: terminal_id_b.clone(),
                event: ascii_key('z', PhysicalKey::Z),
            },
        )
        .await;
        send_frame(
            &mut client_b,
            &FrameKind::InputKey {
                terminal_id: terminal_id_b.clone(),
                event: enter_key(),
            },
        )
        .await;

        let bytes_b = drain_into_screen(&mut client_b, &mut screen_b, "z").await;
        assert!(
            screen_b.contains("z"),
            "client B: post-reconnect 'z' must round-trip through PTY into \
             a fresh TERMINAL_OUTPUT stream. bytes={bytes_b}, screen=\n{}",
            screen_b.snapshot_text(),
        );

        // ============================================================
        // Teardown — drop streams, shut server down, then assert no
        // leaked socket FDs (the file MUST be gone after the server
        // exits) and no leaked PTY (server join MUST complete cleanly).
        // ============================================================
        drop(client_b);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server didn't shut down")
            .expect("server join")
            .expect("server run_async ok");

        // FD-leak guard: the socket file must be unlinked on clean
        // shutdown. If the server leaked the listener FD or skipped
        // its cleanup path, the file would stick around in `tmp` until
        // the TempDir drop. This is a cheap cross-platform smoke test;
        // a precise `/proc/self/fd` count is in `count_open_fds`.
        assert!(
            !socket_path.exists(),
            "socket file leaked after shutdown: {} still on disk",
            socket_path.display(),
        );

        // Linux-only precise FD-leak guard. macOS dev hosts skip this
        // (no `/proc`); the smoke test above is the cross-platform
        // fallback. We can't compare *exactly* against a pre-test count
        // because nextest, tokio, libghostty, and the test binary all
        // hold steady-state FDs — but `count_open_fds()` is monotonic
        // and a value within a sane band (< 256) catches a runaway PTY
        // / socket leak which would push us into the thousands.
        #[cfg(target_os = "linux")]
        {
            let open = count_open_fds();
            assert!(
                open < 256,
                "FD leak suspected: {open} open fds after teardown \
                 (expected < 256 for a single-scenario test binary)",
            );
        }
    });
}

/// Count entries in `/proc/self/fd`. Linux-only. Each entry is one open
/// FD owned by the current process. Used as a leak guard after teardown.
///
/// We use this as a band check (`< 256`), not an exact-equality check:
/// nextest, libghostty's static state, tokio's reactor and the test
/// binary all hold steady-state FDs that vary by toolchain version.
/// What we want to catch is a *runaway* leak — a PTY master not closed,
/// a UDS socket still listening — which manifests as hundreds of stuck
/// FDs, not single-digit drift.
#[cfg(target_os = "linux")]
fn count_open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("read /proc/self/fd")
        .filter_map(Result::ok)
        .count()
}
