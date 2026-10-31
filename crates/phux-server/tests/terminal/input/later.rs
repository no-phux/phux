//! Input tests, continued.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "shares the parent test namespace"
)]
use super::*;

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
            cell_px: None,
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
    let shell = phux_terminal::actor::resolve_shell(None);
    let cmd = phux_terminal::actor::default_shell_command(&shell, false);
    assert_eq!(
        cmd.get_env("TERM").and_then(|t| t.to_str()),
        Some("xterm-256color")
    );
}
