//! Wire input reaching real PTYs: `INPUT_KEY`/`ROUTE_INPUT` ordering and
//! legacy encoding, mouse wheel, routed paste bracketing and trust, and
//! `ROUTE_INPUT`'s no-resize contract.

use std::time::Duration;

use phux_protocol::ids::ResourceId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::{ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{Command, CommandResult, FrameKind};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::tracing_capture::TracingCapture;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, ServerHandles, WIRE_RECV_TIMEOUT, ascii_key, attach_by_name, command,
    join_after_shutdown, recv_until, run_local, send_frame, spawn_server_with_seed_cmd,
    try_recv_typed, wait_for_socket,
};

use super::common::{focused_resource, named_key, poll_screen, sh};

/// A server seeding session `work` with `cmd`, plus one HELLO'd client.
async fn seeded(tmp: &TempDir, cmd: CommandBuilder) -> (ServerHandles, UnixStream) {
    let socket = tmp.path().join("phux.sock");
    let server = spawn_server_with_seed_cmd(socket.clone(), "work", cmd);
    (
        server,
        wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await,
    )
}

/// Attach to `work` and drain through `BOOTSTRAP_READY`; returns the pane.
async fn attach(stream: &mut UnixStream) -> ResourceId {
    send_frame(stream, &attach_by_name("work")).await;
    let pane = recv_until(stream, |_, frame| match frame {
        FrameKind::Attached { snapshot, .. } => Some(snapshot.resources[0].id.clone()),
        _ => None,
    })
    .await;
    recv_until(stream, |_, f| {
        matches!(f, FrameKind::BootstrapReady { .. }).then_some(())
    })
    .await;
    pane
}

async fn route(stream: &mut UnixStream, request_id: u32, pane: &ResourceId, event: InputEvent) {
    let cmd = Command::RouteInput {
        terminal_id: pane.clone(),
        event,
    };
    assert_eq!(command(stream, request_id, cmd).await, CommandResult::Ok);
}

fn key(terminal_id: &ResourceId, c: char, k: PhysicalKey) -> FrameKind {
    FrameKind::InputKey {
        terminal_id: terminal_id.clone(),
        event: ascii_key(c, k),
    }
}

/// `INPUT_KEY`, `ROUTE_INPUT`, `INPUT_KEY` sent back to back share one FIFO
/// into the pane, and plain letters reach a default-mode pane as legacy
/// ASCII (phux-7vx: never kitty CSI-u unless the app opts in), so `cat`
/// echoes `abc` contiguously.
#[test]
fn mixed_input_key_and_route_input_preserve_wire_order() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let ((shutdown, server), mut stream) = seeded(&tmp, CommandBuilder::new("/bin/cat")).await;
        let pane = attach(&mut stream).await;

        send_frame(&mut stream, &key(&pane, 'a', PhysicalKey::A)).await;
        let route_b = Command::RouteInput {
            terminal_id: pane.clone(),
            event: InputEvent::Key(ascii_key('b', PhysicalKey::B)),
        };
        let request = FrameKind::Command {
            request_id: 7,
            command: route_b,
        };
        send_frame(&mut stream, &request).await;
        send_frame(&mut stream, &key(&pane, 'c', PhysicalKey::C)).await;
        let enter = FrameKind::InputKey {
            terminal_id: pane.clone(),
            event: named_key(PhysicalKey::Enter),
        };
        send_frame(&mut stream, &enter).await;

        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        let (mut acc, mut acked) = (Vec::new(), false);
        while !(acked && acc.windows(3).any(|w| w == b"abc")) {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let Ok(Some((_, frame))) = timeout(remaining, try_recv_typed(&mut stream)).await else {
                panic!("no ordered `abc` echo (acked={acked}): {acc:?}");
            };
            match frame {
                FrameKind::ResourceOutput { bytes, .. } => acc.extend_from_slice(&bytes),
                FrameKind::CommandResult {
                    request_id: 7,
                    result,
                } => {
                    assert_eq!(result, CommandResult::Ok);
                    acked = true;
                }
                _ => {}
            }
        }
        assert!(
            !acc.windows(3).any(|w| w == b"[97"),
            "kitty CSI-u leaked: {acc:?}"
        );

        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}

/// A burst of `INPUT_KEY` frames that lands in one socket read reaches the
/// PTY whole. Typed text (the desktop's `commitText`, an IME commit) is one
/// frame per scalar, so a 256-character line arrives as one buffered read.
/// The server must hand the pane actor a turn between those frames: routing
/// them all in one poll overflows the actor's 64-deep encoded-input mailbox
/// and the input lane drops everything past it.
#[test]
fn a_buffered_input_key_burst_reaches_the_pty_whole() {
    const LETTERS: [(char, PhysicalKey); 5] = [
        ('a', PhysicalKey::A),
        ('b', PhysicalKey::B),
        ('c', PhysicalKey::C),
        ('d', PhysicalKey::D),
        ('e', PhysicalKey::E),
    ];
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let ((shutdown, server), mut stream) = seeded(&tmp, CommandBuilder::new("/bin/cat")).await;
        let pane = attach(&mut stream).await;

        let typed: String = (0..256).map(|i| LETTERS[i % LETTERS.len()].0).collect();
        let mut burst = Vec::new();
        for i in 0..typed.len() {
            let (c, k) = LETTERS[i % LETTERS.len()];
            burst.extend_from_slice(&phux_server_testkit::encode_frame(&key(&pane, c, k)));
        }
        let enter = FrameKind::InputKey {
            terminal_id: pane.clone(),
            event: named_key(PhysicalKey::Enter),
        };
        burst.extend_from_slice(&phux_server_testkit::encode_frame(&enter));
        tokio::io::AsyncWriteExt::write_all(&mut stream, &burst)
            .await
            .unwrap();

        // Inside `try_recv_typed`'s own per-read timeout, so a lost tail
        // reports what did arrive rather than a bare read timeout.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut acc = Vec::new();
        while !acc.windows(typed.len()).any(|w| w == typed.as_bytes()) {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let Ok(Some((_, frame))) = timeout(remaining, try_recv_typed(&mut stream)).await else {
                panic!(
                    "the {}-key burst did not reach the PTY whole; echoed {} bytes: {:?}",
                    typed.len(),
                    acc.len(),
                    String::from_utf8_lossy(&acc)
                );
            };
            if let FrameKind::ResourceOutput { bytes, .. } = frame {
                acc.extend_from_slice(&bytes);
            }
        }

        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}

/// phux-yyex: a wheel `INPUT_MOUSE` must encode to an SGR scroll report once
/// the pane enables mouse tracking. Before the fix the encoder had no cell
/// geometry and emitted zero bytes for every mouse event.
#[test]
fn wheel_input_mouse_reaches_a_mouse_tracking_pane() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let tracking = sh("printf '\\033[?1000h\\033[?1002h\\033[?1003h\\033[?1006h'; exec cat");
        let ((shutdown, _server), mut stream) = seeded(&tmp, tracking).await;
        let pane = attach(&mut stream).await;

        // (80, 80) px is cell (10, 5) at the default 8x16 cell: `<64;11;6M`.
        // Resend until the mirror has parsed the modes; earlier wheels are
        // (correctly) dropped.
        let wheel = FrameKind::InputMouse {
            terminal_id: pane,
            event: MouseEvent {
                action: MouseAction::Press,
                button: MouseButton::Four,
                mods: ModSet::empty(),
                x: 80.0,
                y: 80.0,
            },
        };
        let needle: &[u8] = b"[<64;11;6M";
        let mut acc = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !acc.windows(needle.len()).any(|w| w == needle) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "wheel never reached the PTY: {:?}",
                String::from_utf8_lossy(&acc),
            );
            send_frame(&mut stream, &wheel).await;
            let window = tokio::time::Instant::now() + Duration::from_millis(250);
            while let Some(remaining) = window.checked_duration_since(tokio::time::Instant::now())
                && let Ok(frame) = timeout(remaining, try_recv_typed(&mut stream)).await
            {
                let (_, frame) = frame.expect("server closed the connection");
                if let FrameKind::ResourceOutput { bytes, .. } = frame {
                    acc.extend_from_slice(&bytes);
                }
            }
        }
        drop(shutdown);
    });
}

/// `ROUTE_INPUT` carries no viewport, so unlike `ATTACH` it never resizes
/// the pane (phux-3j3); it is accepted from a headless, never-attached
/// caller (the agent path) as well as from an attached one.
#[test]
fn route_input_delivers_keys_without_resizing_the_pane() {
    run_local(async {
        // Dumps the server log on failure: a lost input byte on a 2-core
        // runner has been seen here (phux-dacb).
        let _cap = TracingCapture::install("route_input_no_resize");
        let tmp = TempDir::new().unwrap();
        let (_server, mut headless) = seeded(&tmp, CommandBuilder::new("cat")).await;
        let pane = focused_resource(&mut headless, 1).await;

        let mut attached =
            wait_for_socket(&tmp.path().join("phux.sock"), SOCKET_CONNECT_DEADLINE).await;
        attach(&mut attached).await;
        let resize = FrameKind::ResizeTerminal {
            terminal_id: pane.clone(),
            cols: 120,
            rows: 40,
        };
        send_frame(&mut attached, &resize).await;
        let dims = |s: &phux_core::screen::ScreenState| (s.cols, s.rows) == (120, 40);
        assert!(dims(&poll_screen(&mut headless, &pane, dims).await));

        route(
            &mut headless,
            2,
            &pane,
            InputEvent::Key(ascii_key('q', PhysicalKey::Q)),
        )
        .await;
        route(
            &mut attached,
            3,
            &pane,
            InputEvent::Key(ascii_key('z', PhysicalKey::Z)),
        )
        .await;
        route(
            &mut attached,
            4,
            &pane,
            InputEvent::Key(named_key(PhysicalKey::Enter)),
        )
        .await;

        let after = poll_screen(&mut headless, &pane, |s| {
            dims(s) && s.lines.concat().contains("qz")
        })
        .await;
        assert_eq!(
            (after.cols, after.rows),
            (120, 40),
            "ROUTE_INPUT must not resize"
        );
        assert!(
            after.lines.concat().contains("qz"),
            "routed keys echo: {:?}",
            after.lines
        );
    });
}

/// Routed paste (phux-foir) is bracketed iff the pane enabled DEC 2004, and
/// an untrusted unsafe payload is dropped by the default `Reject` policy
/// while still acking `Ok`. The input mailbox is FIFO, so a trusted marker
/// routed afterwards landing without the unsafe payload proves the drop.
/// The canonical-mode echo renders ESC as `^[`.
#[test]
fn routed_paste_honors_dec_2004_and_trust() {
    let paste = |trust, data: &[u8]| {
        InputEvent::Paste(PasteEvent {
            trust,
            data: data.to_vec(),
        })
    };
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (_server, mut stream) = seeded(&tmp, sh("printf '\\033[?2004hREADY\\n'; cat")).await;
        let pane = focused_resource(&mut stream, 1).await;
        poll_screen(&mut stream, &pane, |s| s.lines.concat().contains("READY")).await;
        route(
            &mut stream,
            2,
            &pane,
            paste(PasteTrust::Trusted, b"bpayload"),
        )
        .await;
        let text = poll_screen(&mut stream, &pane, |s| s.lines.concat().contains("201~")).await;
        assert!(
            text.lines.concat().contains("^[[200~bpayload^[[201~"),
            "a DEC-2004 pane gets bracketed paste: {:?}",
            text.lines,
        );
    });
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (_server, mut stream) = seeded(&tmp, sh("printf 'READY\\n'; cat")).await;
        let pane = focused_resource(&mut stream, 1).await;
        poll_screen(&mut stream, &pane, |s| s.lines.concat().contains("READY")).await;
        route(
            &mut stream,
            2,
            &pane,
            paste(PasteTrust::Untrusted, b"evilpayload\nsecond"),
        )
        .await;
        route(
            &mut stream,
            3,
            &pane,
            paste(PasteTrust::Trusted, b"rawpayload"),
        )
        .await;
        let text = poll_screen(&mut stream, &pane, |s| {
            s.lines.concat().contains("rawpayload")
        })
        .await;
        let text = text.lines.concat();
        assert!(text.contains("rawpayload"), "raw paste lands: {text:?}");
        assert!(
            !text.contains("200~"),
            "mode 2004 off: no brackets: {text:?}"
        );
        assert!(
            !text.contains("evilpayload"),
            "untrusted unsafe paste dropped: {text:?}"
        );
    });
}

/// phux-7vx: ghostty's terminfo advertises `fullkbd`, which makes ncurses apps
/// (htop) push kitty flags they cannot parse back. The default shell must
/// stay on `xterm-256color` until that is proven safe.
#[test]
fn default_shell_command_advertises_xterm_256color() {
    let shell = phux_server::terminal_actor::resolve_shell(None);
    let cmd = phux_server::terminal_actor::default_shell_command(&shell, false);
    assert_eq!(
        cmd.get_env("TERM").and_then(|t| t.to_str()),
        Some("xterm-256color")
    );
}
