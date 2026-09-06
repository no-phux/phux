//! Regression test for `phux-7vx`: keys typed by the user must reach the
//! inner program as **legacy** xterm bytes (plain ASCII for printables,
//! standard CSI for arrows/function keys) unless that inner program has
//! itself opted into the kitty keyboard protocol or xterm's
//! `modifyOtherKeys`.
//!
//! User repro: `phux attach demo` → `htop` → `q` → no quit. The
//! hypothesis was that phux's per-pane encoder was emitting `\x1b[113;1u`
//! (kitty CSI-u) for plain `q` because TERM=ghostty advertises kitty
//! support. htop doesn't speak kitty; it never sees a recognizable quit
//! key.
//!
//! The encoder unit tests in [`key_encode_snapshot`] already pin down the
//! encoder's behavior at the libghostty boundary. This test pins the
//! end-to-end wire path: a `INPUT_KEY` frame for plain `q` (no mods) sent
//! through `handle_client` → `TerminalActor::encode_input` → PTY writer →
//! `cat` echo → `TERMINAL_OUTPUT` MUST contain the byte 0x71 (`q`) and MUST
//! NOT contain a CSI-u escape, **provided** the pane's libghostty
//! Terminal is in its default (legacy) keyboard mode.
//!
//! A second test confirms the same path produces CSI-u for `q` after an
//! application explicitly enables kitty mode by writing `CSI > 31 u`
//! into the pane (this is what an app like neovim does when it wants
//! the progressive keyboard protocol). This is the "principle of opt-in"
//! check: phux must not flip kitty mode on speculatively; it must follow
//! whatever the inner app asks for.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    FrameKind, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN, TYPE_TERMINAL_OUTPUT,
};
use phux_server::input::key::PerTerminalKeyEncoder;
use phux_server::terminal_actor::default_shell_command;
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd, try_recv_typed, wait_for_socket,
};

/// Build a `KeyEvent` for an ASCII printable. Mirrors the shape used by
/// `input_dispatch.rs::ascii_key`.
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

const fn ctrl_c_key() -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::C,
        mods: ModSet::CTRL,
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: Some(b'c' as u32),
    }
}

const fn arrow_up_key() -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::ArrowUp,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

/// Drain `TERMINAL_OUTPUT` frames into a `Vec<u8>` until `needle` appears or
/// the deadline elapses. Returns whatever has accumulated either way.
async fn collect_pane_output_until(stream: &mut UnixStream, needle: u8) -> Vec<u8> {
    let mut acc: Vec<u8> = Vec::new();
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
            acc.extend_from_slice(&bytes);
            if acc.contains(&needle) {
                return acc;
            }
        }
    }
    acc
}

/// End-to-end check: a plain `q` keypress on a freshly attached pane
/// (whose inner program is `cat`, which has not enabled any keyboard
/// protocol modes) must arrive at the PTY as a single byte `0x71` and
/// echo back as such — NOT as `\x1b[113;1u` or other CSI-u variant.
///
/// This is the canonical regression for `phux-7vx`. If a future change
/// flips the per-pane Terminal into kitty mode at startup (e.g., by
/// pre-seeding a `CSI > N u` into `vt_write`, or by setting kitty flags
/// directly via libghostty FFI), this test fails fast.
#[test]
fn plain_q_press_round_trips_as_legacy_ascii_byte() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // `cat` is the deterministic echo fixture: cooked-mode PTY +
        // line-buffered cat → bytes come back after Enter. Same shape
        // as `input_dispatch.rs`.
        let cmd = CommandBuilder::new("/bin/cat");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // ATTACH
        send_frame(&mut stream, &attach_by_name("default")).await;

        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED);
        let wire_pane_id = match attached {
            FrameKind::Attached { snapshot, .. } => {
                assert_eq!(snapshot.panes.len(), 1);
                snapshot.panes[0].id.clone()
            }
            other => panic!("expected ATTACHED, got {other:?}"),
        };
        let (type_byte, _snap) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_BOOTSTRAP_BEGIN);

        // Press `q` then Enter so cat's line buffer flushes.
        send_frame(
            &mut stream,
            &FrameKind::InputKey {
                terminal_id: wire_pane_id.clone(),
                event: ascii_key('q', PhysicalKey::Q),
            },
        )
        .await;
        send_frame(
            &mut stream,
            &FrameKind::InputKey {
                terminal_id: wire_pane_id.clone(),
                event: KeyEvent {
                    action: KeyAction::Press,
                    key: PhysicalKey::Enter,
                    mods: ModSet::empty(),
                    consumed_mods: ModSet::empty(),
                    composing: false,
                    text: None,
                    unshifted_codepoint: None,
                },
            },
        )
        .await;

        let acc = collect_pane_output_until(&mut stream, b'q').await;

        // The accumulated bytes must contain the literal `q` (0x71) —
        // that's the legacy plain-ASCII shape htop expects.
        assert!(
            acc.contains(&b'q'),
            "expected plain `q` byte in TERMINAL_OUTPUT echo, got {acc:?}",
        );

        // And the bytes must NOT contain a CSI-u quit-key encoding. The
        // canonical kitty CSI-u for plain `q` is ESC `[` `113` `u` (with
        // optional `;1` modifier section). If any of these substrings
        // appears, the encoder went rogue.
        let bad_patterns: &[&[u8]] = &[b"\x1b[113u", b"\x1b[113;1u", b"\x1b[113;1:1u"];
        for pat in bad_patterns {
            assert!(
                !acc.windows(pat.len()).any(|w| w == *pat),
                "TERMINAL_OUTPUT contained kitty CSI-u encoding {pat:?}; \
                 raw bytes={acc:?}",
            );
        }

        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// End-to-end check: Ctrl-C in legacy mode must arrive as the single
/// byte `0x03` (ETX), the SIGINT-generating byte the kernel line
/// discipline recognises. If the encoder produced `\x1b[99;5u` (kitty
/// CSI-u for Ctrl-C) instead, ncurses apps would not see a quit signal
/// and the cooked-mode tty would not raise SIGINT.
#[test]
fn ctrl_c_round_trips_as_legacy_etx_byte() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // `cat` shows the byte we sent because in cooked mode the line
        // discipline echoes typed bytes. Ctrl-C is special: cooked mode
        // intercepts it before cat sees it (sends SIGINT to cat), so
        // we don't observe an echo back. We assert on the encoder
        // path indirectly: the bytes we'd write to the PTY for Ctrl-C
        // are validated by the unit/snapshot test layer. Here we
        // simply confirm the wire-driven path doesn't panic and the
        // server stays alive — the byte-shape assertion happens in
        // `encoder_emits_legacy_bytes_for_q_in_default_terminal_mode`
        // below.
        let cmd = CommandBuilder::new("/bin/cat");
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
        let (type_byte, _snap) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_BOOTSTRAP_BEGIN);

        send_frame(
            &mut stream,
            &FrameKind::InputKey {
                terminal_id: wire_pane_id.clone(),
                event: ctrl_c_key(),
            },
        )
        .await;

        // We allow either: no TERMINAL_OUTPUT at all (cat killed before it
        // could echo) or some bytes that DON'T contain a CSI-u
        // encoding of Ctrl-C. The bug-shape we're guarding against is
        // CSI-u showing up here.
        // Ctrl-C kills `cat` (SIGINT in cooked mode), which is this
        // session's only pane. Under the tmux server-exit model
        // (phux-60s) that reaps the session and the server self-exits,
        // dropping the connection. So this loop must tolerate a clean
        // EOF as well as the read deadline — both end accumulation.
        let mut acc: Vec<u8> = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline - tokio::time::Instant::now();
            let Ok(maybe) = timeout(remaining, try_recv_typed(&mut stream)).await else {
                break;
            };
            let Some((type_byte, frame)) = maybe else {
                break; // server self-exited and closed the connection
            };
            if type_byte == TYPE_TERMINAL_OUTPUT
                && let FrameKind::TerminalOutput { bytes, .. } = frame
            {
                acc.extend_from_slice(&bytes);
            }
        }
        // No CSI-u substring for Ctrl-C (`\x1b[99;5u`).
        let bad: &[u8] = b"\x1b[99;5u";
        assert!(
            !acc.windows(bad.len()).any(|w| w == bad),
            "Ctrl-C produced kitty CSI-u shape on a default-mode pane: {acc:?}",
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        let _ = timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, server_handle).await;
    });
}

/// Direct encoder-level corroboration of the integration-test assertion
/// above: a fresh libghostty `Terminal` (matching what `TerminalActor::build`
/// constructs at startup) puts the encoder in legacy mode, so `q` is
/// `0x71`, Ctrl-C is `0x03`, and `ArrowUp` is `\x1b[A`.
#[test]
fn encoder_emits_legacy_bytes_for_q_in_default_terminal_mode() {
    let terminal = GhosttyTerminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 1000,
    })
    .expect("Terminal::new");
    let mut enc = PerTerminalKeyEncoder::new().expect("encoder");

    let q_bytes = enc
        .encode(&ascii_key('q', PhysicalKey::Q), &terminal)
        .expect("encode q")
        .to_vec();
    assert_eq!(q_bytes, b"q", "plain `q` must be a single 0x71 byte");

    let ctrl_c_bytes = enc
        .encode(&ctrl_c_key(), &terminal)
        .expect("encode ^C")
        .to_vec();
    assert_eq!(
        ctrl_c_bytes, b"\x03",
        "Ctrl-C must be ETX (0x03), got {ctrl_c_bytes:?}",
    );

    let up_bytes = enc
        .encode(&arrow_up_key(), &terminal)
        .expect("encode up")
        .to_vec();
    assert_eq!(
        up_bytes, b"\x1b[A",
        "ArrowUp in default mode must be CSI A, got {up_bytes:?}",
    );
}

/// Pins `TERM` to a value that does NOT advertise the kitty keyboard
/// protocol to the spawned child. See phux-7vx: under TERM=ghostty,
/// the ghostty terminfo entry exports the `fullkbd` extended capability
/// that ncurses applications (notably htop) read as "kitty progressive
/// enhancement is safe to enable here," at which point those apps push
/// `CSI > N u` into the pane and then fail to round-trip CSI-u key
/// reports for keys they own (htop's `q` quit being the canonical
/// regression). `xterm-256color` advertises the standard xterm key
/// vocabulary only; apps that explicitly want kitty mode still get it
/// via runtime opt-in, and the encoder pivots accordingly (see
/// `encoder_emits_kitty_csi_u_when_terminal_has_kitty_flags`).
///
/// This test exists to keep the TERM choice load-bearing: a future
/// change that silently flips it back to ghostty (or anything else
/// advertising `fullkbd`) must update this test deliberately.
#[test]
fn default_shell_command_advertises_xterm_256color() {
    let cmd = default_shell_command(&phux_server::terminal_actor::resolve_shell(None), false);
    let term = cmd
        .get_env("TERM")
        .map(|s| s.to_string_lossy().into_owned())
        .expect("default_shell_command must set TERM");
    assert_eq!(
        term, "xterm-256color",
        "TERM must be xterm-256color to avoid the htop CSI-u regression (phux-7vx). \
         If you have explicit support for kitty keyboard round-trip across all \
         supported apps, update this test deliberately.",
    );
}

/// Opt-in side: once the inner app writes `CSI > 31 u` (push kitty
/// progressive enhancement, all flags) into the pane's Terminal, the
/// encoder MUST switch to CSI-u for the same `q` press. This pins down
/// "phux respects the app's request" — the inverse of the
/// regression. If this test starts failing the encoder has lost its
/// terminal-state awareness; if the *previous* test starts failing the
/// pane is starting in kitty mode (the actual `phux-7vx` regression
/// shape).
#[test]
fn encoder_emits_kitty_csi_u_when_terminal_has_kitty_flags() {
    let mut terminal = GhosttyTerminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 1000,
    })
    .expect("Terminal::new");
    // Push kitty flags (DISAMBIGUATE | REPORT_EVENTS | REPORT_ALTERNATES
    // | REPORT_ALL | REPORT_ASSOCIATED == 31). An inner app that wants
    // the full kitty progressive enhancement writes exactly this.
    terminal.vt_write(b"\x1b[>31u");

    let mut enc = PerTerminalKeyEncoder::new().expect("encoder");
    let q_bytes = enc
        .encode(&ascii_key('q', PhysicalKey::Q), &terminal)
        .expect("encode q")
        .to_vec();
    // The exact CSI-u shape can vary across libghostty revisions; the
    // important invariants are: starts with ESC `[`, ends with `u`, and
    // is NOT a single plain `q`.
    assert_ne!(
        q_bytes, b"q",
        "with kitty flags pushed, plain `q` must NOT be legacy 0x71"
    );
    assert!(
        q_bytes.starts_with(b"\x1b["),
        "kitty-mode `q` must be a CSI sequence, got {q_bytes:?}",
    );
    assert_eq!(
        q_bytes.last().copied(),
        Some(b'u'),
        "kitty-mode `q` must terminate with `u`, got {q_bytes:?}",
    );
}
