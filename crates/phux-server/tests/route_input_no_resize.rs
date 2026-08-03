//! Wire-level integration tests for the `ROUTE_INPUT` command dispatch
//! seam (`phux-3j3`, `phux-nlo`).
//!
//! `ROUTE_INPUT` delivers an already-built input event to a Terminal
//! without advertising a viewport and without resizing. The bug it closes
//! off is the last disruptive side effect on the agent surface —
//! `send-keys`/`run` used to `ATTACH` with an `80x24` viewport, which
//! transiently resized the live pane.
//!
//! `ROUTE_INPUT` is also PRIMARY-only input authority (SPEC input.md §7 /
//! L1.md §7.1). With no materialized per-connection role map yet, PRIMARY
//! is approximated by an active subscription: a caller that never attached
//! is a viewer and is rejected with `PERMISSION_DENIED` (phux-nlo).
//!
//! These tests drive the real `handle_client` read loop end-to-end (no
//! TUI, no tmux). The no-resize test proves the dimension invariant:
//!
//! 1. Pre-seed a session whose pane is backed by a real PTY running
//!    `cat` (cooked-mode echo gives a crisp signal).
//! 2. Resolve the pane id via the side-effect-free `GET_STATE` (no attach).
//! 3. On a routing connection, `ATTACH` (to gain PRIMARY) then size the
//!    pane to `120x40` with `TERMINAL_RESIZE` so the explicit live
//!    dimensions exceed any attach viewport.
//! 4. `ROUTE_INPUT` a key + Enter — `ROUTE_INPUT` advertises no viewport.
//! 5. `GET_SCREEN` again: the pane MUST still report `120x40`, not the
//!    `80x24` the attach viewport carried, and the echoed byte must have
//!    landed.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(
    clippy::doc_markdown,
    reason = "test narrative uses bare wire-frame names (ROUTE_INPUT, GET_SCREEN, …)"
)]

mod common;

use std::time::Duration;

use phux_protocol::input::InputEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, StateScope, TYPE_ATTACHED,
    TYPE_BOOTSTRAP_BEGIN, TYPE_COMMAND_RESULT,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use crate::common::tracing_capture::TracingCapture;
use crate::common::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// Build a press `KeyEvent` for an ASCII printable.
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

/// Enter — no `text`; libghostty's encoder synthesizes the CR.
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

/// Drain frames until a `COMMAND_RESULT` with `request_id` arrives.
async fn await_command_result(stream: &mut UnixStream, request_id: u32) -> CommandResult {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if type_byte != TYPE_COMMAND_RESULT {
            continue;
        }
        if let FrameKind::CommandResult {
            request_id: got,
            result,
        } = frame
            && got == request_id
        {
            return result;
        }
    }
    panic!("no COMMAND_RESULT with request_id={request_id} within deadline");
}

/// `GET_STATE { Server }` → the focused pane id of the seeded session.
async fn focused_pane(stream: &mut UnixStream, request_id: u32) -> phux_protocol::ids::TerminalId {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::GetState {
                scope: StateScope::Server,
            },
        },
    )
    .await;
    match await_command_result(stream, request_id).await {
        CommandResult::OkWith(CommandValue::State(snap)) => snap.focused_pane,
        other => panic!("expected Ok_With(State(..)), got {other:?}"),
    }
}

/// `ATTACH { ByName(name) }` and drain the opening `ATTACHED` +
/// `TERMINAL_SNAPSHOT` frames. Attaching subscribes the connection to the
/// session's active pane, which is the interim PRIMARY proxy `ROUTE_INPUT`
/// gates on (phux-nlo). The 80x24 attach viewport is intentionally
/// overridden by a later `TERMINAL_RESIZE`.
async fn attach_and_drain(stream: &mut UnixStream, name: &str) {
    send_frame(stream, &attach_by_name(name)).await;
    let (type_byte, frame) = recv_typed(stream).await;
    assert_eq!(type_byte, TYPE_ATTACHED, "first frame must be ATTACHED");
    assert!(
        matches!(frame, FrameKind::Attached { .. }),
        "expected Attached, got {frame:?}",
    );
    let (type_byte, _snap) = recv_typed(stream).await;
    assert_eq!(
        type_byte, TYPE_BOOTSTRAP_BEGIN,
        "ATTACHED must be followed by a TERMINAL_SNAPSHOT",
    );
}

/// `GET_SCREEN` → the pane's current `(cols, rows)` and its joined text.
async fn screen(
    stream: &mut UnixStream,
    request_id: u32,
    terminal_id: &phux_protocol::ids::TerminalId,
) -> phux_core::screen::ScreenState {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::GetScreen {
                terminal_id: terminal_id.clone(),
                request_scrollback: None,
                cells: false,
            },
        },
    )
    .await;
    match await_command_result(stream, request_id).await {
        CommandResult::OkWith(CommandValue::Json(json)) => {
            serde_json::from_str(&json).expect("GET_SCREEN reply must be a valid ScreenState")
        }
        other => panic!("expected Ok_With(Json(..)), got {other:?}"),
    }
}

#[test]
fn route_input_delivers_keys_without_resizing_the_pane() {
    run_local(async {
        // CI-flake forensics: this test has failed on the 2-core runner
        // with an empty screen after the full echo budget — a lost input
        // byte, not a slow one. The capture dumps the server's debug log
        // (pty-writer death, ROUTE_INPUT mailbox drops) on panic so the
        // next occurrence identifies the seam instead of re-opening the
        // guessing game (phux-dacb history).
        let _cap = TracingCapture::install("route_input_no_resize");
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (_shutdown_tx, _server) =
            spawn_server_with_seed_cmd(socket_path.clone(), "work", CommandBuilder::new("cat"));
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // Resolve the pane id without attaching.
        let pane = focused_pane(&mut stream, 1).await;

        // Route input on a connection that ATTACHes first — `ROUTE_INPUT`
        // is PRIMARY-only (phux-nlo), and attaching is the interim way to
        // hold PRIMARY. The 80x24 attach viewport is then overridden by an
        // explicit TERMINAL_RESIZE, so any reversion to 80x24 would be a
        // ROUTE_INPUT-induced resize.
        let mut route_conn = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        attach_and_drain(&mut route_conn, "work").await;

        // Size the pane to 120x40 (the dims the agent surface must preserve).
        send_frame(
            &mut route_conn,
            &FrameKind::TerminalResize {
                terminal_id: pane.clone(),
                cols: 120,
                rows: 40,
            },
        )
        .await;

        // Confirm the resize landed before we send any input.
        let before = poll_for_dims(&mut stream, &pane, (120, 40), 10).await;
        assert_eq!(
            (before.cols, before.rows),
            (120, 40),
            "TERMINAL_RESIZE must size the pane to 120x40 before ROUTE_INPUT",
        );

        for (i, key) in [ascii_key('z', PhysicalKey::Z), enter_key()]
            .into_iter()
            .enumerate()
        {
            let request_id = u32::try_from(i).unwrap() + 1;
            send_frame(
                &mut route_conn,
                &FrameKind::Command {
                    request_id,
                    command: Command::RouteInput {
                        terminal_id: pane.clone(),
                        event: InputEvent::Key(key),
                    },
                },
            )
            .await;
            match await_command_result(&mut route_conn, request_id).await {
                CommandResult::Ok => {}
                other => panic!("ROUTE_INPUT must ack with Ok, got {other:?}"),
            }
        }

        // The pane MUST still be 120x40 — ROUTE_INPUT carries no viewport,
        // so unlike the attach path it does not shrink the pane to the
        // attach viewport's 80x24. Poll for the echoed byte to confirm the
        // event actually reached the PTY, then assert the dims held.
        // phux-dacb: generous budget (200 x 25ms = 5s) + require the settled
        // dims, so a slow echo on a loaded CI runner can't trip the dims assert.
        let after = poll_for_echo(&mut stream, &pane, 'z', Some((120, 40)), 200).await;
        assert_eq!(
            (after.cols, after.rows),
            (120, 40),
            "ROUTE_INPUT must NOT resize the pane: expected 120x40, got {}x{}",
            after.cols,
            after.rows,
        );
        let joined: String = after.lines.join("");
        assert!(
            joined.contains('z'),
            "routed key must reach the PTY and echo back; screen text: {joined:?}",
        );
    });
}

/// `ROUTE_INPUT` is the side-effect-free agent path (ADR-0022): `phux run`
/// / `send-keys` resolve a pane via `GET_STATE` and inject input WITHOUT
/// ever attaching or subscribing. So an unsubscribed control-plane caller
/// MUST be accepted (`Ok`) — not rejected. (A prior interim gate, phux-nlo,
/// keyed "PRIMARY" off subscription and rejected exactly this headless
/// path, breaking the agent surface; this pins the corrected behavior.)
/// Genuine viewer-vs-primary authority returns with materialized
/// per-connection roles and must gate an *attached* read-only viewer, never
/// this headless caller.
#[test]
fn route_input_from_unsubscribed_agent_is_accepted() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (_shutdown_tx, _server) =
            spawn_server_with_seed_cmd(socket_path.clone(), "work", CommandBuilder::new("cat"));
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // Resolve the pane id without attaching — GET_STATE does not
        // subscribe, so this connection is the headless agent surface.
        let pane = focused_pane(&mut stream, 1).await;

        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 2,
                command: Command::RouteInput {
                    terminal_id: pane.clone(),
                    event: InputEvent::Key(ascii_key('z', PhysicalKey::Z)),
                },
            },
        )
        .await;
        match await_command_result(&mut stream, 2).await {
            CommandResult::Ok => {}
            other => panic!("headless ROUTE_INPUT must be accepted (Ok), got {other:?}"),
        }
    });
}

/// The PRIMARY counterpart to the viewer rejection above: once a
/// connection ATTACHes (gaining the interim PRIMARY subscription),
/// `ROUTE_INPUT` for that pane is accepted and acks `Ok` (phux-nlo).
#[test]
fn route_input_from_primary_subscriber_succeeds() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (_shutdown_tx, _server) =
            spawn_server_with_seed_cmd(socket_path.clone(), "work", CommandBuilder::new("cat"));
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        let pane = focused_pane(&mut stream, 1).await;

        // ATTACH subscribes this connection to the active pane → PRIMARY.
        let mut primary = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        attach_and_drain(&mut primary, "work").await;

        send_frame(
            &mut primary,
            &FrameKind::Command {
                request_id: 1,
                command: Command::RouteInput {
                    terminal_id: pane.clone(),
                    event: InputEvent::Key(ascii_key('q', PhysicalKey::Q)),
                },
            },
        )
        .await;
        match await_command_result(&mut primary, 1).await {
            CommandResult::Ok => {}
            other => panic!("primary ROUTE_INPUT must ack Ok, got {other:?}"),
        }

        // Confirm the routed byte actually reached the PTY.
        let echoed = poll_for_echo(&mut stream, &pane, 'q', None, 200).await;
        let joined: String = echoed.lines.join("");
        assert!(
            joined.contains('q'),
            "primary's routed key must reach the PTY and echo back; screen text: {joined:?}",
        );
    });
}

/// Poll `GET_SCREEN` until the pane reports `want` dims (or attempts run out).
async fn poll_for_dims(
    stream: &mut UnixStream,
    pane: &phux_protocol::ids::TerminalId,
    want: (u16, u16),
    attempts: u32,
) -> phux_core::screen::ScreenState {
    let mut last = None;
    for i in 0..attempts {
        let s = screen(stream, 1000 + i, pane).await;
        if (s.cols, s.rows) == want {
            return s;
        }
        last = Some(s);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    last.expect("at least one GET_SCREEN")
}

/// Poll `GET_SCREEN` until `needle` appears in the joined screen text.
/// Poll `GET_SCREEN` until BOTH the echoed `needle` is on screen AND the pane
/// reports `dims`, or `attempts` expire. phux-dacb: gating on both conditions
/// (not just the echo) makes the dims assertion robust on a saturated 2-core CI
/// runner — under contention the routed key's PTY echo can lag well past a
/// tight budget, and returning a transient pre-settle snapshot then tripped the
/// dims assert. Returns the first snapshot satisfying both; on timeout returns
/// the last seen so the caller's assertions fail with real diagnostics.
async fn poll_for_echo(
    stream: &mut UnixStream,
    pane: &phux_protocol::ids::TerminalId,
    needle: char,
    dims: Option<(u16, u16)>,
    attempts: u32,
) -> phux_core::screen::ScreenState {
    let mut last = None;
    for i in 0..attempts {
        let s = screen(stream, 2000 + i, pane).await;
        let dims_ok = dims.is_none_or(|d| (s.cols, s.rows) == d);
        if dims_ok && s.lines.iter().any(|l| l.contains(needle)) {
            return s;
        }
        last = Some(s);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    last.expect("at least one GET_SCREEN")
}
