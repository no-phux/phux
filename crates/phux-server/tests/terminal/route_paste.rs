//! Wire-level integration tests for paste events over `ROUTE_INPUT`
//! (phux-foir).
//!
//! `phux paste` is a pure projection: it routes the existing `INPUT_PASTE`
//! atom (`docs/spec/input.md` §5) over the side-effect-free `ROUTE_INPUT`
//! path, and the server's `PerTerminalPasteEncoder` decides bracketed vs
//! raw delivery from the pane's DEC mode 2004 state. These tests prove
//! that seam end to end against a real PTY:
//!
//! 1. A pane whose program switched DEC 2004 on (`printf '\033[?2004h…'`)
//!    receives the payload wrapped in `ESC[200~` / `ESC[201~`. The PTY's
//!    canonical-mode echo renders the ESC byte as the printable `^[`
//!    (ECHOCTL), so the bracket markers are observable as screen text.
//! 2. A pane without the mode receives the raw payload, unbracketed.
//! 3. The trust bit rides the wire: an untrusted-and-unsafe payload is
//!    dropped by the default `Reject` policy (the `ROUTE_INPUT` still acks
//!    `Ok`), proven by ordering against a subsequent trusted paste.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(
    clippy::doc_markdown,
    reason = "test narrative uses bare wire-frame names (ROUTE_INPUT, GET_SCREEN, …)"
)]

use std::time::Duration;

use phux_protocol::input::InputEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, StateScope, TYPE_COMMAND_RESULT,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// Build a `/bin/sh -c SCRIPT` seed command.
fn sh_seed(script: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args(["-c", script]);
    cmd
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

/// `GET_SCREEN` → the pane's viewport rows joined into one string.
async fn screen_text(
    stream: &mut UnixStream,
    request_id: u32,
    terminal_id: &phux_protocol::ids::TerminalId,
) -> String {
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
            let state: phux_core::screen::ScreenState =
                serde_json::from_str(&json).expect("GET_SCREEN reply must be a valid ScreenState");
            state.lines.join("\n")
        }
        other => panic!("expected Ok_With(Json(..)), got {other:?}"),
    }
}

/// Poll `GET_SCREEN` until `needle` appears in the joined screen text, or
/// `attempts` expire. Returns the last text seen so a timeout fails the
/// caller's assertion with real diagnostics. The generous budget
/// (200 x 25ms = 5s) tolerates a saturated CI runner (phux-dacb history).
async fn poll_for_text(
    stream: &mut UnixStream,
    pane: &phux_protocol::ids::TerminalId,
    needle: &str,
    attempts: u32,
) -> String {
    let mut last = String::new();
    for i in 0..attempts {
        last = screen_text(stream, 2000 + i, pane).await;
        if last.contains(needle) {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    last
}

/// Route one paste event to `pane` and require the `Ok` ack.
async fn route_paste(
    stream: &mut UnixStream,
    request_id: u32,
    pane: &phux_protocol::ids::TerminalId,
    trust: PasteTrust,
    data: &[u8],
) {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::RouteInput {
                terminal_id: pane.clone(),
                event: InputEvent::Paste(PasteEvent {
                    trust,
                    data: data.to_vec(),
                }),
            },
        },
    )
    .await;
    match await_command_result(stream, request_id).await {
        CommandResult::Ok => {}
        other => panic!("ROUTE_INPUT paste must ack with Ok, got {other:?}"),
    }
}

/// A pane whose program enabled bracketed paste (DEC mode 2004) receives
/// the routed payload wrapped in ESC[200~ … ESC[201~. The seed prints the
/// mode-set BEFORE the READY marker, so once GET_SCREEN shows the marker,
/// the pane's terminal (and the input encoder snapshot published after the
/// same vt_write) has the mode on. The canonical-mode PTY echoes ESC as
/// the printable `^[`, making the markers assertable as screen text.
#[test]
fn routed_paste_is_bracketed_when_pane_enables_dec_2004() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (_shutdown_tx, _server) = spawn_server_with_seed_cmd(
            socket_path.clone(),
            "work",
            sh_seed("printf '\\033[?2004hBRACKETREADY\\n'; cat"),
        );
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        let pane = focused_pane(&mut stream, 1).await;
        let ready = poll_for_text(&mut stream, &pane, "BRACKETREADY", 200).await;
        assert!(
            ready.contains("BRACKETREADY"),
            "seed program never came up; screen: {ready:?}",
        );

        route_paste(&mut stream, 2, &pane, PasteTrust::Trusted, b"bpayload").await;

        // The echoed input is `^[[200~bpayload^[[201~` — ESC rendered as
        // the two printable characters `^[` by the tty's ECHOCTL echo.
        let text = poll_for_text(&mut stream, &pane, "^[[200~bpayload^[[201~", 200).await;
        assert!(
            text.contains("^[[200~bpayload^[[201~"),
            "paste into a DEC-2004 pane must arrive bracketed; screen: {text:?}",
        );
    });
}

/// A pane that never enabled DEC mode 2004 receives the raw payload — no
/// bracket markers (`PerTerminalPasteEncoder` picks raw delivery when the
/// snapshotted mode is off).
#[test]
fn routed_paste_is_raw_when_dec_2004_is_off() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (_shutdown_tx, _server) = spawn_server_with_seed_cmd(
            socket_path.clone(),
            "work",
            sh_seed("printf 'RAWREADY\\n'; cat"),
        );
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        let pane = focused_pane(&mut stream, 1).await;
        let ready = poll_for_text(&mut stream, &pane, "RAWREADY", 200).await;
        assert!(
            ready.contains("RAWREADY"),
            "seed program never came up; screen: {ready:?}",
        );

        route_paste(&mut stream, 2, &pane, PasteTrust::Trusted, b"rawpayload").await;

        let text = poll_for_text(&mut stream, &pane, "rawpayload", 200).await;
        assert!(
            text.contains("rawpayload"),
            "raw paste must reach the PTY and echo back; screen: {text:?}",
        );
        assert!(
            !text.contains("200~"),
            "paste into a mode-2004-off pane must NOT be bracketed; screen: {text:?}",
        );
    });
}

/// The trust bit is honored server-side: an UNTRUSTED payload that fails
/// `paste::is_safe` (the newline) is dropped by the default `Reject`
/// policy while the `ROUTE_INPUT` still acks `Ok`. The drop is proven by
/// ordering — a subsequent TRUSTED paste on the same connection lands, and
/// the input mailbox is FIFO, so had the unsafe payload been forwarded it
/// would be on screen before the marker.
#[test]
fn untrusted_unsafe_paste_is_dropped_by_default_policy() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (_shutdown_tx, _server) = spawn_server_with_seed_cmd(
            socket_path.clone(),
            "work",
            sh_seed("printf 'TRUSTREADY\\n'; cat"),
        );
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        let pane = focused_pane(&mut stream, 1).await;
        let ready = poll_for_text(&mut stream, &pane, "TRUSTREADY", 200).await;
        assert!(
            ready.contains("TRUSTREADY"),
            "seed program never came up; screen: {ready:?}",
        );

        // Untrusted + multiline ⇒ unsafe ⇒ rejected (silently: still Ok).
        route_paste(
            &mut stream,
            2,
            &pane,
            PasteTrust::Untrusted,
            b"evilpayload\nsecondline",
        )
        .await;
        // Trusted marker paste routed after it, same connection: FIFO.
        route_paste(&mut stream, 3, &pane, PasteTrust::Trusted, b"aftermarker").await;

        let text = poll_for_text(&mut stream, &pane, "aftermarker", 200).await;
        assert!(
            text.contains("aftermarker"),
            "trusted marker paste must land; screen: {text:?}",
        );
        assert!(
            !text.contains("evilpayload"),
            "untrusted unsafe paste must be dropped by the Reject default; screen: {text:?}",
        );
    });
}
