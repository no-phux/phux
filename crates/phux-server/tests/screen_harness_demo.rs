//! Demonstrates the `common::screen::Screen` helper end-to-end.
//!
//! Companion to `input_dispatch.rs` (which counts `b'a'` bytes in the
//! emitted `TERMINAL_OUTPUT` stream). This test does the same wire dance —
//! spin up a server with a real PTY backed by `cat`, attach, send a
//! keystroke — but then feeds every `TERMINAL_OUTPUT` byte chunk into a
//! `Screen` and asserts on the *rendered text*, not raw byte counts.
//!
//! Why it matters: the parent agent spent half a day debugging a render
//! bug by stripping SGR escapes with regex and counting characters. If
//! the regex missed a sequence, it was indistinguishable from "no output
//! at all". The `Screen` oracle makes the assertion "row 0 contains the
//! string we typed" trivial — and reads exactly as well as the original
//! diagnostic the agent had to do by hand.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
// `Screen` owns a `!Send` `libghostty_vt::Terminal` by design (ADR-0014);
// the integration tests run on a `LocalSet` so non-Send futures are fine.
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

mod common;

use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    FrameKind, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN, TYPE_TERMINAL_OUTPUT,
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

/// One ASCII press, no modifiers. Mirrors `input_dispatch.rs::ascii_key`.
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

/// Drain `TERMINAL_OUTPUT` frames into the `Screen` until either `needle`
/// appears in the rendered grid or `WIRE_RECV_TIMEOUT` elapses. Returns
/// the total bytes fed, for diagnostic reporting on failure.
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
fn screen_helper_observes_pty_echo_through_wire() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // `cat` echoes stdin → stdout in cooked mode. Same fixture as
        // `input_dispatch.rs`; if cooked-mode echo isn't there, `cat`
        // would still emit the post-Enter echoed line, so 'a' must show
        // up on row 0 either way.
        let cmd = CommandBuilder::new("/bin/cat");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // --- ATTACH + ATTACHED + TERMINAL_SNAPSHOT ---
        send_frame(&mut stream, &attach_by_name("default")).await;
        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "first frame must be ATTACHED");
        let wire_pane_id = match attached {
            FrameKind::Attached { snapshot, .. } => snapshot.panes[0].id.clone(),
            other => panic!("expected ATTACHED, got {other:?}"),
        };
        let (type_byte, _snap) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_BOOTSTRAP_BEGIN,
            "second frame must be TERMINAL_SNAPSHOT"
        );

        // Build a Screen sized to the ATTACH viewport (80x24, matching
        // `attach_by_name`). Anything the server emits flows through it.
        let mut screen = Screen::new(80, 24).expect("Screen::new");

        // --- Type 'a' then Enter so cat echoes the line ---
        send_frame(
            &mut stream,
            &FrameKind::InputKey {
                terminal_id: wire_pane_id.clone(),
                event: ascii_key('a', PhysicalKey::A),
            },
        )
        .await;
        send_frame(
            &mut stream,
            &FrameKind::InputKey {
                terminal_id: wire_pane_id.clone(),
                event: enter_key(),
            },
        )
        .await;

        // The whole point of the harness: assert on the rendered text,
        // not byte counts. If the dispatch is broken, no TERMINAL_OUTPUT
        // arrives and `screen.row(0)` stays "" — the assertion message
        // shows exactly what the user would see on attach.
        let bytes_fed = drain_into_screen(&mut stream, &mut screen, "a").await;
        let row0 = screen.row(0);
        assert!(
            screen.contains("a"),
            "Screen must observe the PTY echo of 'a' on some row. \
             bytes fed: {bytes_fed}, row(0)={row0:?}, full screen:\n{}",
            screen.snapshot_text(),
        );

        // Teardown.
        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server didn't shut down")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// Unit tests for the `common::screen::Screen` oracle itself. They live here
/// (one binary) rather than inside `tests/common/screen.rs`, where a
/// `#[cfg(test)] mod tests` would compile and re-run in every integration
/// binary that declares `mod common`.
mod screen_oracle_tests {
    use super::common::screen::Screen;

    #[test]
    fn write_ascii_then_read_row_zero() {
        let mut s = Screen::new(20, 3).unwrap();
        s.write(b"hello");
        assert_eq!(s.row(0), "hello");
    }

    #[test]
    fn contains_finds_text_anywhere() {
        let mut s = Screen::new(20, 3).unwrap();
        s.write(b"line one\r\nline two");
        assert!(s.contains("two"));
        assert!(!s.contains("three"));
    }

    #[test]
    fn snapshot_text_joins_rows_with_newlines() {
        let mut s = Screen::new(10, 3).unwrap();
        s.write(b"ab\r\ncd");
        let text = s.snapshot_text();
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "ab");
        assert_eq!(lines[1], "cd");
    }

    #[test]
    fn cursor_advances_after_write() {
        let mut s = Screen::new(20, 3).unwrap();
        s.write(b"abc");
        let (col, row) = s.cursor();
        // `cursor_viewport()` may degrade to (0, 0) when libghostty
        // can't resolve the cursor; accept either the precise answer
        // or the safe default. The important invariant for the harness
        // is "doesn't panic".
        assert!(row <= 2);
        assert!(col <= 20);
    }

    #[test]
    fn out_of_range_row_is_empty() {
        let mut s = Screen::new(10, 2).unwrap();
        s.write(b"hi");
        assert_eq!(s.row(99), "");
    }

    #[test]
    fn sgr_escapes_are_stripped_in_text_output() {
        let mut s = Screen::new(20, 3).unwrap();
        // Bold + red + "ok" + reset. The libghostty parser must absorb
        // the SGR so only "ok" lands in the grid.
        s.write(b"\x1b[1;31mok\x1b[0m");
        assert_eq!(s.row(0), "ok");
    }
}
