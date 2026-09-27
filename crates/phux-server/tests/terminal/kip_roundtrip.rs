//! phux-0o8: kitty keyboard protocol round-trips for real host TUIs under
//! `TERM=ghostty`, driven over the wire (the encoder pivots to CSI-u only when
//! the app pushes kitty flags) and asserted on the rendered screen.
//!
//! The TUIs are host-provided, not nix-pinned, so each test probes for its
//! binary and the `ghostty` terminfo and skips (passing, with a greppable
//! `SKIP(kip_roundtrip::..)` line) when either is missing. Findings
//! (2026-07-10): fzf, less, nvim, vim and btop all pass under ghostty; only
//! nvim pushes kitty flags, so it is the one genuine CSI-u round trip. htop,
//! the phux-7vx ncurses `fullkbd` reproducer, was never available, so the
//! default `TERM` stays `xterm-256color` (`defaults.term` opts in).
//!
//! "The app quit" is the probed pane's `RESOURCE_CLOSED { Exited }`, which
//! needs a second pane in the window (a sole Terminal's exit respawns a shell
//! in place, phux-5y00); see `TuiProbe::pin_anchor_pane`.

#![allow(clippy::print_stderr, reason = "skip markers + KIP forensics")]

use std::path::PathBuf;
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{CloseReason, FrameKind, SpawnResult};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::screen::Screen;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, Spawn, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local,
    seed_pty, send_frame, spawn_server_with, try_recv_typed, wait_for_server_screen_text,
    wait_for_socket,
};

use super::common::{named_key, sh};

/// `bin` on `$PATH` or a conventional install prefix nextest may not have.
fn find_program(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from))
        .map(|dir| dir.join(bin))
        .find(|candidate| candidate.is_file())
}

/// `false` (after printing a skip marker) unless `bin` and the `ghostty`
/// terminfo entry both exist on this host.
fn require_tui(test: &str, bin: &str) -> bool {
    let reason = if find_program(bin).is_none() {
        format!("`{bin}` not installed on this host")
    } else if !std::process::Command::new("infocmp")
        .arg("ghostty")
        .output()
        .is_ok_and(|out| out.status.success())
    {
        "no `ghostty` terminfo entry on this host".to_owned()
    } else {
        return true;
    };
    eprintln!("SKIP(kip_roundtrip::{test}): {reason}");
    false
}

/// One printable-ASCII press, shaped as the attached client translates it
/// from the host TTY (US layout: shifted glyphs carry `Shift` and their
/// unshifted codepoint).
fn printable_key(c: char) -> KeyEvent {
    const SHIFTED: &str = "!1@2#3$4%5^6&7*8(9)0_-+={[}]|\\:;\"'<,>.?/~`";
    let (key, shifted) = match c {
        ' ' => (PhysicalKey::Space, false),
        '0'..='9' => (
            PhysicalKey::try_from(6 + (c as u32 - '0' as u32)).unwrap(),
            false,
        ),
        'a'..='z' => (
            PhysicalKey::try_from(20 + (c as u32 - 'a' as u32)).unwrap(),
            false,
        ),
        'A'..='Z' => (
            PhysicalKey::try_from(20 + (c as u32 - 'A' as u32)).unwrap(),
            true,
        ),
        _ => phux_config::keybind::punct_to_key(c).unwrap_or_else(|| panic!("no key for {c:?}")),
    };
    let chars: Vec<char> = SHIFTED.chars().collect();
    let unshifted = chars
        .chunks(2)
        .find(|pair| pair[0] == c)
        .map_or_else(|| c.to_ascii_lowercase(), |pair| pair[1]);
    let mods = if shifted {
        ModSet::SHIFT
    } else {
        ModSet::empty()
    };
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods,
        consumed_mods: mods,
        composing: false,
        text: Some(c.to_string()),
        unshifted_codepoint: Some(unshifted as u32),
    }
}

/// A wire-attached probe around one TUI-in-a-pane: the probed pane's output
/// feeds a [`Screen`] oracle and is kept raw for kitty forensics.
struct TuiProbe {
    stream: UnixStream,
    socket_path: PathBuf,
    terminal_id: ResourceId,
    screen: Screen,
    raw: Vec<u8>,
    closed: Option<(CloseReason, Option<i32>)>,
    transport_eof: bool,
}

impl TuiProbe {
    /// Attach and replay the bootstrap into the oracle (output that raced the
    /// attach arrives only there), then pin the anchor pane.
    async fn attach(mut stream: UnixStream, socket_path: PathBuf) -> Self {
        send_frame(&mut stream, &attach_by_name("default")).await;
        let terminal_id = match recv_typed(&mut stream).await.1 {
            FrameKind::Attached { snapshot, .. } => snapshot.resources[0].id.clone(),
            other => panic!("expected ATTACHED, got {other:?}"),
        };
        let mut probe = Self {
            stream,
            socket_path,
            terminal_id,
            screen: Screen::new(80, 24).unwrap(),
            raw: Vec::new(),
            closed: None,
            transport_eof: false,
        };
        loop {
            match recv_typed(&mut probe.stream).await.1 {
                FrameKind::BootstrapChunk { payload, .. } => {
                    probe.raw.extend_from_slice(&payload);
                    probe.screen.write(&payload);
                }
                FrameKind::BootstrapReady { .. } => break,
                _ => {}
            }
        }
        probe.pin_anchor_pane().await;
        probe
    }

    /// Spawn an inert `/bin/cat` into the probed pane's window so the TUI's
    /// natural exit closes its pane instead of respawning a shell in place
    /// (and the session never empties, so the server cannot self-exit).
    async fn pin_anchor_pane(&mut self) {
        let spawn = Spawn {
            owner_terminal: Some(self.terminal_id.clone()),
            ..Spawn::command(&["/bin/cat"])
        };
        send_frame(&mut self.stream, &spawn.frame(9001)).await;
        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        // Pump rather than skip: the probed pane is already painting.
        while let Some(frame) = self.pump_once(deadline).await {
            if let FrameKind::ResourceSpawned {
                request_id: 9001,
                result,
            } = frame
            {
                assert!(matches!(result, SpawnResult::Ok(_)), "anchor: {result:?}");
                return;
            }
        }
        panic!("anchor pane never spawned");
    }

    async fn send_key(&mut self, event: KeyEvent) {
        let frame = FrameKind::InputKey {
            terminal_id: self.terminal_id.clone(),
            event,
        };
        send_frame(&mut self.stream, &frame).await;
    }

    async fn type_str(&mut self, s: &str) {
        for c in s.chars() {
            self.send_key(printable_key(c)).await;
        }
    }

    /// Pump one frame, folding the probed pane's output into the oracle.
    /// `None` once the pane closed, the transport died, or `deadline` passed.
    async fn pump_once(&mut self, deadline: tokio::time::Instant) -> Option<FrameKind> {
        if self.closed.is_some() || self.transport_eof {
            return None;
        }
        let remaining = deadline.checked_duration_since(tokio::time::Instant::now())?;
        let Some((_, frame)) = timeout(remaining, try_recv_typed(&mut self.stream))
            .await
            .ok()?
        else {
            self.transport_eof = true;
            return None;
        };
        match &frame {
            FrameKind::ResourceOutput {
                terminal_id, bytes, ..
            } if *terminal_id == self.terminal_id => {
                self.raw.extend_from_slice(bytes);
                self.screen.write(bytes);
            }
            FrameKind::ResourceClosed {
                terminal_id,
                exit_status,
                reason,
                ..
            } if *terminal_id == self.terminal_id => {
                self.closed = Some((*reason, *exit_status));
                return None;
            }
            _ => {}
        }
        Some(frame)
    }

    async fn expect_screen_contains(&mut self, needle: &str, what: &str) {
        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        while !self.screen.contains(needle) {
            let progressed = self.pump_once(deadline).await.is_some();
            assert!(
                progressed || self.screen.contains(needle),
                "{what}: {needle:?} never appeared.\n--- screen ---\n{}",
                self.screen.snapshot_text()
            );
        }
    }

    /// Poll the server's own grid on a separate unsubscribed connection (the
    /// attached stream's output must keep feeding the oracle).
    async fn wait_server_screen_text(&self, needle: &str, deadline: Duration) {
        let mut control = wait_for_socket(&self.socket_path, SOCKET_CONNECT_DEADLINE).await;
        wait_for_server_screen_text(&mut control, &self.terminal_id, needle, deadline).await;
    }

    /// The app really quit: its pane closed as `Exited`, not killed or torn
    /// down. The exit status races a 20ms reap budget, so it is only reported.
    async fn expect_closed(&mut self, what: &str) {
        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        while self.pump_once(deadline).await.is_some() {}
        let screen = self.screen.snapshot_text();
        assert!(!self.transport_eof, "{what}: transport dropped.\n{screen}");
        let Some((reason, status)) = self.closed else {
            panic!("{what}: pane never closed.\n{screen}");
        };
        assert_eq!(
            reason,
            CloseReason::Exited,
            "{what} (status {status:?}).\n{screen}"
        );
        eprintln!(
            "kip_roundtrip({what}): exit status {status:?}, kitty push {}, kitty query {}",
            contains_csi_u(&self.raw, b'>'),
            contains_csi_u(&self.raw, b'?'),
        );
    }
}

/// `ESC [ <intro> <digits;:> u`: a kitty keyboard push (`>`) or query (`?`).
fn contains_csi_u(haystack: &[u8], intro: u8) -> bool {
    haystack.windows(3).enumerate().any(|(i, w)| {
        w == [0x1b, b'[', intro]
            && haystack[i + 3..]
                .iter()
                .find(|b| !(b.is_ascii_digit() || **b == b';' || **b == b':'))
                == Some(&b'u')
    })
}

/// Seed a pane running `cmd` under `TERM=<term>`, attach, run `scenario`.
fn run_tui_probe<F>(cmd: CommandBuilder, term: &str, scenario: F)
where
    F: AsyncFnOnce(&mut TuiProbe),
{
    run_local(async move {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, _server) =
            spawn_server_with(socket_path.clone(), Some("default"), |cfg| {
                seed_pty(cfg, cmd);
                term.clone_into(&mut cfg.term);
            });
        let stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        let mut probe = TuiProbe::attach(stream, socket_path).await;
        scenario(&mut probe).await;
        drop(shutdown_tx);
    });
}

/// Every ghostty run below depends on `defaults.term` reaching the seed
/// pane's environment; otherwise they silently become xterm control runs.
#[test]
fn harness_seed_pane_sees_configured_term() {
    let cmd = sh("printf 'TERM_IS[%s]' \"$TERM\"; sleep 5");
    run_tui_probe(cmd, "ghostty", async |probe: &mut TuiProbe| {
        probe
            .expect_screen_contains("TERM_IS[ghostty]", "seed TERM")
            .await;
    });
}

#[test]
fn fzf_filters_and_accepts_under_term_ghostty() {
    if !require_tui("fzf_ghostty", "fzf") {
        return;
    }
    let cmd = sh("printf 'alpha\\nbravo\\ncharlie\\n' | fzf");
    run_tui_probe(cmd, "ghostty", async |probe: &mut TuiProbe| {
        probe.expect_screen_contains("charlie", "fzf list").await;
        probe.type_str("brav").await;
        probe.expect_screen_contains("> brav", "fzf query").await;
        probe
            .expect_screen_contains("1/3", "fzf match counter")
            .await;
        probe.send_key(named_key(PhysicalKey::Enter)).await;
        probe.expect_closed("fzf accept").await;
    });
}

#[test]
fn less_searches_and_quits_under_term_ghostty() {
    if !require_tui("less_ghostty", "less") {
        return;
    }
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("numbers.txt");
    let body: Vec<String> = (1..=200).map(|n| format!("line-{n}")).collect();
    std::fs::write(&file, body.join("\n")).unwrap();
    let mut cmd = CommandBuilder::new("less");
    cmd.arg(&file);
    run_tui_probe(cmd, "ghostty", async |probe: &mut TuiProbe| {
        probe.expect_screen_contains("line-1", "less page").await;
        probe.type_str("/line-137").await;
        probe
            .expect_screen_contains("/line-137", "less search prompt")
            .await;
        probe.send_key(named_key(PhysicalKey::Enter)).await;
        probe.expect_screen_contains("line-137", "less jump").await;
        probe.send_key(printable_key('q')).await;
        probe.expect_closed("less quit").await;
    });
}

/// nvim pushes kitty flags, so every key after startup crosses the wire as
/// CSI-u and must be parsed back: the genuine KIP round trip.
#[test]
fn nvim_kip_insert_and_quit_under_term_ghostty() {
    if !require_tui("nvim_ghostty", "nvim") {
        return;
    }
    let mut cmd = CommandBuilder::new("nvim");
    cmd.arg("--clean");
    run_tui_probe(cmd, "ghostty", async |probe: &mut TuiProbe| {
        probe
            .expect_screen_contains("[No Name]", "nvim startup")
            .await;
        probe.send_key(printable_key('i')).await;
        probe.type_str("kip roundtrip ok").await;
        probe
            .expect_screen_contains("kip roundtrip ok", "nvim insert")
            .await;
        probe.send_key(named_key(PhysicalKey::Escape)).await;
        probe.type_str(":q!").await;
        probe.expect_screen_contains(":q!", "nvim cmdline").await;
        probe.send_key(named_key(PhysicalKey::Enter)).await;
        probe.expect_closed("nvim :q!").await;
    });
}

/// Hang guard for vim under CPU starvation (phux-7y78), not a latency bound.
const VIM_HANG_GUARD: Duration = Duration::from_secs(60);

#[test]
fn vim_insert_and_quit_under_term_ghostty() {
    if !require_tui("vim_ghostty", "vim") {
        return;
    }
    let mut cmd = CommandBuilder::new("vim");
    cmd.args(["-u", "NONE", "-i", "NONE"]);
    run_tui_probe(cmd, "ghostty", async |probe: &mut TuiProbe| {
        // A lone `i` on the splash is eaten as wait_return; Enter first.
        probe
            .wait_server_screen_text("VIM - Vi IMproved", VIM_HANG_GUARD)
            .await;
        probe.send_key(named_key(PhysicalKey::Enter)).await;
        probe.send_key(printable_key('i')).await;
        probe.type_str("kip roundtrip ok").await;
        probe
            .wait_server_screen_text("kip roundtrip ok", VIM_HANG_GUARD)
            .await;
        probe.send_key(named_key(PhysicalKey::Escape)).await;
        probe.type_str(":q!").await;
        probe.expect_screen_contains(":q!", "vim cmdline").await;
        probe.send_key(named_key(PhysicalKey::Enter)).await;
        probe.expect_closed("vim :q!").await;
    });
}

#[test]
fn btop_quits_on_q_under_term_ghostty() {
    if !require_tui("btop_ghostty", "btop") {
        return;
    }
    let mut cmd = CommandBuilder::new("btop");
    // btop refuses to start without a UTF-8 locale.
    cmd.env("LANG", "en_US.UTF-8");
    cmd.env("LC_ALL", "en_US.UTF-8");
    run_tui_probe(cmd, "ghostty", async |probe: &mut TuiProbe| {
        probe.expect_screen_contains("cpu", "btop dashboard").await;
        probe.send_key(printable_key('q')).await;
        probe.expect_closed("btop quit").await;
    });
}
