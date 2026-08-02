//! Wire-level integration test for dedicated input-lane dispatch.
//!
//! A real UDS client attaches to a PTY-backed `cat`, then writes an
//! `INPUT_KEY`, `ROUTE_INPUT`, and another `INPUT_KEY` without waiting for the
//! command acknowledgement. The echoed `abc` proves both input surfaces use
//! one FIFO from the client read loop through pane delivery.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

mod common;

use phux_protocol::input::InputEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    Command, CommandResult, FrameKind, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::time::timeout;

use crate::common::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// Build a `KeyEvent` for an ASCII printable matching `phux-byc.6.5`'s
/// fixture shape: press, no modifiers, no composition.
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

/// Build an Enter key — no `text`, libghostty's encoder synthesizes the
/// CR. Matches `pty_pump.rs::input_keystroke_reaches_pty_and_echoes_back`.
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

#[test]
fn mixed_input_key_and_route_input_preserve_wire_order() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // `cat` is the deterministic echo fixture: cooked-mode PTY +
        // line-buffered cat → `a\r\n` (or similar) comes back after we
        // send `a` then Enter. Mirrors pty_pump.rs's fixture exactly.
        let cmd = CommandBuilder::new("/bin/cat");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // ---- ATTACH ----
        send_frame(&mut stream, &attach_by_name("default")).await;

        // ---- ATTACHED ----
        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ATTACHED,
            "first server-to-client frame must be ATTACHED",
        );
        let wire_pane_id = match attached {
            FrameKind::Attached { snapshot, .. } => {
                assert_eq!(snapshot.panes.len(), 1, "exactly one pane");
                snapshot.panes[0].id.clone()
            }
            other => panic!("expected ATTACHED, got {other:?}"),
        };

        // ---- TERMINAL_SNAPSHOT (one per pane in focused window) ----
        let (type_byte, _snap_frame) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_BOOTSTRAP_BEGIN,
            "second server-to-client frame must be TERMINAL_SNAPSHOT",
        );

        // Send mixed data-plane and control-plane input without waiting for
        // the ROUTE_INPUT acknowledgement. The per-client read loop must put
        // every event on one dedicated-lane FIFO in this wire order.
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
            &FrameKind::Command {
                request_id: 7,
                command: Command::RouteInput {
                    terminal_id: wire_pane_id.clone(),
                    event: InputEvent::Key(ascii_key('b', PhysicalKey::B)),
                },
            },
        )
        .await;
        send_frame(
            &mut stream,
            &FrameKind::InputKey {
                terminal_id: wire_pane_id.clone(),
                event: ascii_key('c', PhysicalKey::C),
            },
        )
        .await;
        send_frame(
            &mut stream,
            &FrameKind::InputKey {
                terminal_id: wire_pane_id,
                event: enter_key(),
            },
        )
        .await;

        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        let mut acc = Vec::new();
        let mut route_acked = false;
        while tokio::time::Instant::now() < deadline
            && (!acc.windows(3).any(|window| window == b"abc") || !route_acked)
        {
            let remaining = deadline - tokio::time::Instant::now();
            let Ok((_type_byte, frame)) = timeout(remaining, recv_typed(&mut stream)).await else {
                break;
            };
            match frame {
                FrameKind::TerminalOutput { bytes, .. } => acc.extend_from_slice(&bytes),
                FrameKind::CommandResult {
                    request_id: 7,
                    result,
                } => {
                    assert_eq!(result, CommandResult::Ok, "ROUTE_INPUT must succeed");
                    route_acked = true;
                }
                _ => {}
            }
        }
        assert!(
            route_acked,
            "ROUTE_INPUT must receive its correlated Ok result"
        );
        assert!(
            acc.windows(3).any(|window| window == b"abc"),
            "mixed INPUT_KEY/ROUTE_INPUT frames must reach the PTY in wire order (got {} bytes: {:?})",
            acc.len(),
            acc,
        );

        // Clean teardown.
        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}
