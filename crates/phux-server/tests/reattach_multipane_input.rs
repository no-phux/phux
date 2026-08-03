//! `phux-fysb` regression — re-attaching to a MULTI-PANE session must let
//! the re-attached client type into EVERY pane, not just the active one.
//!
//! The bug: `ServerState::attach` used to subscribe the attaching client to
//! only the session's ACTIVE pane. The input gate in `handle_terminal_input`
//! DROPS any `INPUT_KEY` for a pane the client isn't subscribed to ("client
//! not subscribed to pane; dropping input"). So on (re-)attach to a session
//! with more than one pane, the client could see every pane's prompt but
//! could only type into the active one — a freshly spawned pane worked solely
//! because `handle_spawn_terminal` auto-subscribes the spawner. The user hit
//! this as "still can't type after re-attach" once the off-loop-writer fix
//! (`#51`) made the panes render again.
//!
//! The fix subscribes the attaching client to every pane across all the
//! session's windows. This pins it at the wire level, the way the user hits
//! it — the previous coverage was a `ServerState` unit test only, and the fix
//! was nearly lost in the `state.rs` module-split refactor.
//!
//! Shape:
//!
//! 1. Seed a session whose pane runs `cat` (cooked-mode echo = crisp signal).
//! 2. Client A attaches and `SPAWN_TERMINAL`s a second `cat` pane into the
//!    same session, then drops — leaving a persisted two-pane session.
//! 3. Client B attaches fresh (the re-attach). With the fix it is subscribed
//!    to BOTH panes.
//! 4. A side observer (never attached; `GET_STATE`/`GET_SCREEN` are
//!    side-effect-free and unsubscribed) resolves the focused (active) pane
//!    and the non-active one.
//! 5. B `INPUT_KEY`s into the active pane (baseline: input delivery works at
//!    all) and into the NON-active pane (the regression guard). The observer
//!    reads each pane's mirror via `GET_SCREEN` and both echoes must land —
//!    reading the mirror, not B's own output stream, isolates "the keystroke
//!    reached the PTY" from "B happens to be subscribed to output".
//!
//! Without the fix the non-active keystroke is dropped, its echo never reaches
//! the pane mirror, and the second assertion fails.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(
    clippy::doc_markdown,
    reason = "test narrative uses bare wire-frame names (INPUT_KEY, GET_SCREEN, …)"
)]

use std::time::Duration;

use phux_protocol::ids::TerminalId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, SpawnResult, StateScope, TYPE_ATTACHED,
    TYPE_COMMAND_RESULT, TYPE_TERMINAL_SPAWNED,
};
use phux_server::DEFAULT_GROUP_ID;
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// Press `KeyEvent` for an ASCII printable. Matches the other wire tests'
/// fixture so the encoding is identical.
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

/// Drain frames until a `TERMINAL_SPAWNED` with `request_id` arrives.
async fn await_terminal_spawned(stream: &mut UnixStream, request_id: u32) -> SpawnResult {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if type_byte != TYPE_TERMINAL_SPAWNED {
            continue;
        }
        if let FrameKind::TerminalSpawned {
            request_id: got,
            result,
        } = frame
            && got == request_id
        {
            return result;
        }
    }
    panic!("timed out waiting for TERMINAL_SPAWNED request_id={request_id}");
}

/// Send `ATTACH { ByName(name) }` and read the opening `ATTACHED` frame,
/// asserting the attach took. Any following `TERMINAL_SNAPSHOT`s are left in
/// the socket buffer — callers here observe state through a side connection,
/// not this stream's own output.
async fn attach_read_attached(stream: &mut UnixStream, name: &str) {
    send_frame(stream, &attach_by_name(name)).await;
    let (type_byte, frame) = recv_typed(stream).await;
    assert_eq!(type_byte, TYPE_ATTACHED, "first frame must be ATTACHED");
    assert!(
        matches!(frame, FrameKind::Attached { .. }),
        "expected Attached, got {frame:?}",
    );
}

/// `GET_STATE { Server }` → the session's pane ids and its focused (active)
/// pane.
async fn panes_and_focus(
    stream: &mut UnixStream,
    request_id: u32,
) -> (Vec<TerminalId>, TerminalId) {
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
        CommandResult::OkWith(CommandValue::State(snap)) => {
            let panes = snap.panes.iter().map(|p| p.id.clone()).collect();
            (panes, snap.focused_pane)
        }
        other => panic!("expected Ok_With(State(..)), got {other:?}"),
    }
}

/// `GET_SCREEN` for `pane` → its joined screen text.
async fn screen_text(stream: &mut UnixStream, request_id: u32, pane: &TerminalId) -> String {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::GetScreen {
                terminal_id: pane.clone(),
                request_scrollback: None,
                cells: false,
            },
        },
    )
    .await;
    match await_command_result(stream, request_id).await {
        CommandResult::OkWith(CommandValue::Json(json)) => {
            let snap: phux_core::screen::ScreenState =
                serde_json::from_str(&json).expect("GET_SCREEN reply must be a valid ScreenState");
            snap.lines.join("")
        }
        other => panic!("expected Ok_With(Json(..)), got {other:?}"),
    }
}

/// Poll `GET_SCREEN` on `pane` until `needle` appears (or attempts run out).
async fn poll_for_echo(
    stream: &mut UnixStream,
    pane: &TerminalId,
    needle: char,
    attempts: u32,
) -> String {
    let mut last = String::new();
    for i in 0..attempts {
        last = screen_text(stream, 3000 + i, pane).await;
        if last.contains(needle) {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    last
}

/// Send an ASCII key + Enter to `pane` over `stream` as `INPUT_KEY` frames —
/// the TUI client's input path (the one the subscription gate guards).
async fn type_into(stream: &mut UnixStream, pane: &TerminalId, c: char, key: PhysicalKey) {
    send_frame(
        stream,
        &FrameKind::InputKey {
            terminal_id: pane.clone(),
            event: ascii_key(c, key),
        },
    )
    .await;
    send_frame(
        stream,
        &FrameKind::InputKey {
            terminal_id: pane.clone(),
            event: enter_key(),
        },
    )
    .await;
}

#[test]
fn reattach_to_multipane_session_can_type_into_non_active_pane() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // Seed pane runs `cat`: cooked-mode echo gives a crisp per-pane signal.
        let (_shutdown_tx, _server) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", CommandBuilder::new("cat"));

        // Observer connection — never attaches. GET_STATE / GET_SCREEN are
        // side-effect-free and unsubscribed, so it reads each pane's mirror
        // independently of who holds the subscription. That is what proves a
        // keystroke reached the PTY, not merely that B got output.
        let mut obs = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // Client A builds the two-pane session: attach, then split a second
        // `cat` pane into the same session (auto-subscribed via the spawn).
        let mut a = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        attach_read_attached(&mut a, "default").await;
        send_frame(
            &mut a,
            &FrameKind::SpawnTerminal {
                request_id: 1,
                group: DEFAULT_GROUP_ID,
                command: Some(vec!["cat".to_owned()]),
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            },
        )
        .await;
        match await_terminal_spawned(&mut a, 1).await {
            SpawnResult::Ok(id) => assert!(id.is_local(), "spawned pane must be LOCAL"),
            other => panic!("SPAWN_TERMINAL did not succeed: {other:?}"),
        }

        // Confirm the session really has two panes before we drop A.
        let (panes_before, _focus_before) = panes_and_focus(&mut obs, 10).await;
        assert_eq!(
            panes_before.len(),
            2,
            "the seed pane + the spawned pane must both live in the session",
        );

        // A goes away — the panes (live `cat`s) keep the session alive (tmux
        // model; self-exit only fires when a pane dies), so B re-attaches to a
        // persisted multi-pane session.
        drop(a);

        // Client B re-attaches fresh. With the fix it is subscribed to BOTH
        // panes; without it, only the active one.
        let mut b = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        attach_read_attached(&mut b, "default").await;

        // Resolve which pane is active vs. not — the regression is specific to
        // the NON-active pane, and the fix is independent of which pane that is.
        let (panes, active) = panes_and_focus(&mut obs, 11).await;
        assert_eq!(
            panes.len(),
            2,
            "session must still have two panes after re-attach"
        );
        let non_active = panes
            .iter()
            .find(|p| **p != active)
            .cloned()
            .expect("a non-active pane must exist in a two-pane session");

        // Baseline: typing into the ACTIVE pane works at all. If this fails the
        // harness itself is broken, not the subscription fix.
        type_into(&mut b, &active, 'q', PhysicalKey::Q).await;
        let active_text = poll_for_echo(&mut obs, &active, 'q', 40).await;
        assert!(
            active_text.contains('q'),
            "baseline: INPUT_KEY into the active pane must reach the PTY; got {active_text:?}",
        );

        // The regression guard: typing into the NON-active pane must ALSO reach
        // the PTY. Pre-fix this keystroke was dropped by the subscription gate.
        type_into(&mut b, &non_active, 'z', PhysicalKey::Z).await;
        let non_active_text = poll_for_echo(&mut obs, &non_active, 'z', 40).await;
        assert!(
            non_active_text.contains('z'),
            "phux-fysb: INPUT_KEY into a NON-active pane after re-attach must reach the PTY \
             (a re-attached client must be subscribed to every pane, not just the active one); \
             got {non_active_text:?}",
        );
    });
}
