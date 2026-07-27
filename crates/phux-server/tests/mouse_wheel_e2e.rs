//! End-to-end regression for `phux-yyex`: an `INPUT_MOUSE` wheel event
//! sent by a client must reach the inner program as encoded mouse bytes.
//!
//! Before the fix, `PerTerminalMouseEncoder` never configured libghostty's
//! `EncoderSize`, and with zero cell geometry the encoder emits zero bytes
//! for EVERY mouse event — so the server silently discarded all mouse
//! input at `service_input`'s "encoded to zero bytes" branch. A user could
//! not scroll any mouse-tracking app (Claude Code, htop, vim) inside phux.
//!
//! The pane program here mimics a mouse-tracking TUI: it enables the same
//! DECSET set Claude Code was probed to use (`?1000h ?1002h ?1003h
//! ?1006h`), then `exec cat`s so the PTY stays alive. Once the server's
//! Terminal mirror has parsed those modes, a wheel-up `INPUT_MOUSE` must
//! encode to an SGR scroll report (`ESC [ < 64 ; col ; row M`) and land on
//! the PTY, where the cooked-mode line discipline echoes it back to us as
//! `TERMINAL_OUTPUT` (ESC echoes as caret notation, so we assert on the
//! `[<64;` body).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

mod common;

use std::time::Duration;

use phux_protocol::input::key::ModSet;
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::wire::frame::{FrameKind, TYPE_ATTACHED, TYPE_TERMINAL_OUTPUT};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::time::timeout;

use crate::common::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, try_recv_typed, wait_for_socket,
};

#[test]
fn wheel_input_mouse_reaches_a_mouse_tracking_pane() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // Claude Code's probed DECSET set, then an echo fixture. The
        // printf output flows PTY -> actor `vt_write` -> the server-side
        // mirror flips its mouse-tracking modes, exactly as a real TUI
        // would.
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args([
            "-c",
            "printf '\\033[?1000h\\033[?1002h\\033[?1003h\\033[?1006h'; exec cat",
        ]);
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("default")).await;
        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED);
        let wire_pane_id = match attached {
            FrameKind::Attached { snapshot, .. } => snapshot.panes[0].id.clone(),
            other => panic!("expected ATTACHED, got {other:?}"),
        };

        // Wheel-up at surface pixels (80, 80): cell (10, 5) under the
        // server's default 8x16 cell geometry, so the SGR report reads
        // `<64;11;6M` (1-based). The mode-enabling printf races the pump,
        // and a wheel encoded before the modes land is (correctly)
        // dropped, so send-and-check in a retry loop: once the mirror has
        // the modes, one wheel produces the echo.
        let wheel = FrameKind::InputMouse {
            terminal_id: wire_pane_id.clone(),
            event: MouseEvent {
                action: MouseAction::Press,
                button: MouseButton::Four,
                mods: ModSet::empty(),
                x: 80.0,
                y: 80.0,
            },
        };
        let needle: &[u8] = b"[<64;11;6M";
        let mut acc: Vec<u8> = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut found = false;
        'outer: while tokio::time::Instant::now() < deadline {
            send_frame(&mut stream, &wheel).await;
            let drain_until = tokio::time::Instant::now() + Duration::from_millis(250);
            while tokio::time::Instant::now() < drain_until {
                let remaining = drain_until - tokio::time::Instant::now();
                let Ok(maybe) = timeout(remaining, try_recv_typed(&mut stream)).await else {
                    break;
                };
                let Some((type_byte, frame)) = maybe else {
                    panic!("server closed the connection while waiting for the wheel echo");
                };
                if type_byte == TYPE_TERMINAL_OUTPUT
                    && let FrameKind::TerminalOutput { bytes, .. } = frame
                {
                    acc.extend_from_slice(&bytes);
                    if acc.windows(needle.len()).any(|w| w == needle) {
                        found = true;
                        break 'outer;
                    }
                }
            }
        }
        assert!(
            found,
            "wheel INPUT_MOUSE never reached the PTY as SGR bytes; \
             accumulated output: {:?}",
            String::from_utf8_lossy(&acc),
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        let _ = timeout(crate::common::SERVER_JOIN_DEADLINE, server_handle).await;
    });
}
