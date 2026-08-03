//! phux-0o8: kitty-keyboard-protocol (KIP) round-trip harness for real,
//! host-provided TUIs under `TERM=ghostty`.
//!
//! Context: phux-7vx swapped the default `TERM` from `ghostty` to
//! `xterm-256color` because ghostty's terminfo advertises the `fullkbd`
//! extended capability, ncurses TUIs (htop being the canonical reproducer)
//! push the kitty progressive-enhancement flags (`CSI > N u`) on startup,
//! libghostty's per-pane encoder then correctly pivots to CSI-u for every
//! keypress — and htop does not parse incoming CSI-u for the keys it owns,
//! so `q` stops quitting. phux-ign later made the default a config knob
//! (`defaults.term`) plus a per-spawn wire field. phux-0o8 asks: does the
//! full phux stack round-trip KIP for representative TUIs, and if so,
//! should the default flip back to `ghostty`?
//!
//! This file is the automated half of that evidence. Each test drives the
//! REAL wire path — `handle_client` → per-Terminal key encoder (which
//! pivots to CSI-u if and only if the inner app pushed kitty flags into
//! the pane's libghostty `Terminal`) → PTY → the actual TUI binary — and
//! asserts an observable reaction (rendered query text, a search jump, a
//! clean quit) through the `Screen` VT oracle.
//!
//! HONESTY CONSTRAINTS, deliberately load-bearing:
//!
//! - The TUIs are host-provided, NOT nix-pinned (`flake.nix` declares none
//!   of them). Every test therefore probes for its binary and for the
//!   `ghostty` terminfo entry at runtime and SKIPS (passing, with a loud
//!   `SKIP(kip_roundtrip)` line on stderr) when either is missing. In the
//!   nix CI sandbox these tests skip; they only bite on developer hosts
//!   that have the apps. That makes them evidence + regression tooling,
//!   not a reproducible CI proof — which is exactly why the findings
//!   below, not these tests alone, justify the default-TERM decision.
//! - htop — the canonical phux-7vx reproducer — is not installed on the
//!   machine this harness was developed on. Its probe is `#[ignore]`d
//!   rather than skipped-by-probe so that running it is always a
//!   deliberate act; see `htop_quits_on_q_under_term_ghostty`.
//!
//! FINDINGS (2026-07-10, macOS host, all probes green on first run):
//!
//! | app  | version        | kitty query | kitty push | reactions under ghostty |
//! |------|----------------|-------------|------------|-------------------------|
//! | fzf  | 0.74.0 (brew)  | no          | no         | pass                    |
//! | less | 668 (/usr/bin) | no          | no         | pass                    |
//! | nvim | 0.12.4 (brew)  | yes         | yes        | pass                    |
//! | vim  | 9.1 (/usr/bin) | yes         | no         | pass                    |
//! | btop | 1.4.7 (brew)   | no          | no         | pass                    |
//! | htop | NOT INSTALLED  | —           | —          | UNTESTED                |
//!
//! - nvim is the only app observed pushing kitty progressive enhancement
//!   (`CSI > … u` in its output stream), i.e. the only one that actually
//!   exercised the CSI-u encoder pivot end-to-end: after its push, every
//!   key below (`kip roundtrip ok`, Esc, `:q!`, Enter) went to nvim as
//!   CSI-u and nvim parsed all of them back. That is a genuine
//!   full-stack KIP round-trip through phux.
//! - vim 9.1 *queried* KIP (`CSI ? u`) but did not push; fzf/less/btop
//!   showed no kitty activity at all. For those four, the ghostty run
//!   proves "no regression under TERM=ghostty", not "KIP works".
//! - Crucially, none of the five is an ncurses `fullkbd` consumer. The
//!   phux-7vx failure mode was specifically ncurses reading `fullkbd`
//!   from the ghostty terminfo and pushing flags the app cannot parse
//!   back — and the app on that path (htop) could NOT be tested (not
//!   installed, not nix-pinned).
//!
//! DECISION: the default stays `TERM=xterm-256color`. The evidence bar
//! for flipping was "ALL representative TUIs round-trip cleanly", and the
//! canonical regression app is untested — flipping on this evidence would
//! be gambling exactly where we already lost once. Users who want
//! ghostty's extended terminfo have two deliberate opt-ins: server-wide
//! `defaults.term = "ghostty"` in config, or the per-spawn
//! `SPAWN_TERMINAL.term` wire field. See `docs/consumers/tui.md` §4.2 and
//! `crates/phux-config/src/default.toml`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
// `Screen` owns a `!Send` `libghostty_vt::Terminal` (ADR-0014); these
// tests run on a `LocalSet` so non-Send futures are fine.
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]
// This harness reports runtime skips (host-provided binaries) and
// kitty-activity forensics on stderr — that reporting is the point.
#![allow(clippy::print_stderr, reason = "skip markers + KIP forensics")]

use std::path::PathBuf;

use phux_protocol::ids::TerminalId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{
    FrameKind, TYPE_ATTACHED, TYPE_TERMINAL_CLOSED, TYPE_TERMINAL_OUTPUT, TYPE_TERMINAL_SNAPSHOT,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::screen::Screen;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, recv_typed, run_local, send_frame,
    spawn_server_with_seed_cmd_and_term, try_recv_typed, wait_for_socket,
};

// ---------------------------------------------------------------------------
// Host probes: this harness depends on host-provided binaries and terminfo.
// ---------------------------------------------------------------------------

/// Locate `bin` on `$PATH`, falling back to the conventional macOS /
/// Linux install prefixes nextest might not have on its `PATH`.
fn find_program(bin: &str) -> Option<PathBuf> {
    let path_hits = std::env::var_os("PATH").map(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(bin))
            .collect::<Vec<_>>()
    });
    let fallbacks = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]
        .into_iter()
        .map(|dir| PathBuf::from(dir).join(bin));
    path_hits
        .into_iter()
        .flatten()
        .chain(fallbacks)
        .find(|candidate| candidate.is_file())
}

/// `true` when the host terminfo database has an entry for `ghostty`.
/// Without it the TUIs under test refuse to start (`Error opening
/// terminal: ghostty`), which would test nothing about phux.
fn ghostty_terminfo_available() -> bool {
    std::process::Command::new("infocmp")
        .arg("ghostty")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Print a loud, greppable skip marker. The test then returns early and
/// PASSES — see the module docs for why runtime-skip is the honest shape
/// for host-provided (non-nix-pinned) fixtures.
fn skip(test: &str, reason: &str) {
    eprintln!("SKIP(kip_roundtrip::{test}): {reason}");
}

/// Probe for one TUI + the ghostty terminfo entry; `None` means skip
/// (already reported to stderr).
fn require_tui(test: &str, bin: &str) -> Option<PathBuf> {
    let Some(path) = find_program(bin) else {
        skip(test, &format!("`{bin}` not installed on this host"));
        return None;
    };
    if !ghostty_terminfo_available() {
        skip(test, "no `ghostty` terminfo entry on this host");
        return None;
    }
    Some(path)
}

// ---------------------------------------------------------------------------
// Key-event builders: mirror the client's `c0_or_ascii_to_key` translation
// (crates/phux-client/src/attach/input.rs) so the wire events here are
// byte-for-byte the shape a real attached client produces.
// ---------------------------------------------------------------------------

/// Map an ASCII letter (either case) to its [`PhysicalKey`].
fn letter_to_physical(upper: u8) -> PhysicalKey {
    match upper {
        b'A' => PhysicalKey::A,
        b'B' => PhysicalKey::B,
        b'C' => PhysicalKey::C,
        b'D' => PhysicalKey::D,
        b'E' => PhysicalKey::E,
        b'F' => PhysicalKey::F,
        b'G' => PhysicalKey::G,
        b'H' => PhysicalKey::H,
        b'I' => PhysicalKey::I,
        b'J' => PhysicalKey::J,
        b'K' => PhysicalKey::K,
        b'L' => PhysicalKey::L,
        b'M' => PhysicalKey::M,
        b'N' => PhysicalKey::N,
        b'O' => PhysicalKey::O,
        b'P' => PhysicalKey::P,
        b'Q' => PhysicalKey::Q,
        b'R' => PhysicalKey::R,
        b'S' => PhysicalKey::S,
        b'T' => PhysicalKey::T,
        b'U' => PhysicalKey::U,
        b'V' => PhysicalKey::V,
        b'W' => PhysicalKey::W,
        b'X' => PhysicalKey::X,
        b'Y' => PhysicalKey::Y,
        b'Z' => PhysicalKey::Z,
        other => panic!("not an ASCII uppercase letter: {other:#x}"),
    }
}

/// Physical key + implied-Shift for a printable ASCII byte. Mirrors the
/// client's `ascii_to_physical` / `ascii_shift_mods` pair: punctuation
/// routes through the same `phux_config::keybind::punct_to_key` table the
/// chord parser uses.
fn printable_to_physical(c: char) -> (PhysicalKey, bool) {
    match c {
        ' ' => (PhysicalKey::Space, false),
        '0' => (PhysicalKey::Digit0, false),
        '1' => (PhysicalKey::Digit1, false),
        '2' => (PhysicalKey::Digit2, false),
        '3' => (PhysicalKey::Digit3, false),
        '4' => (PhysicalKey::Digit4, false),
        '5' => (PhysicalKey::Digit5, false),
        '6' => (PhysicalKey::Digit6, false),
        '7' => (PhysicalKey::Digit7, false),
        '8' => (PhysicalKey::Digit8, false),
        '9' => (PhysicalKey::Digit9, false),
        'a'..='z' => (letter_to_physical(c.to_ascii_uppercase() as u8), false),
        'A'..='Z' => (letter_to_physical(c as u8), true),
        _ => phux_config::keybind::punct_to_key(c)
            .unwrap_or_else(|| panic!("no PhysicalKey mapping for {c:?}")),
    }
}

/// What this key would produce with no modifiers held (US layout), for
/// `unshifted_codepoint`. Mirrors the client's `ascii_unshifted`.
const fn ascii_unshifted(c: char) -> char {
    match c {
        'A'..='Z' => c.to_ascii_lowercase(),
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        '~' => '`',
        other => other,
    }
}

/// One printable-ASCII press, exactly as the attached client would
/// translate it from the host TTY.
fn printable_key(c: char) -> KeyEvent {
    let (key, shifted) = printable_to_physical(c);
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
        unshifted_codepoint: Some(ascii_unshifted(c) as u32),
    }
}

/// A named (no-text) key press: Enter, Escape, arrows, …
const fn named_key(key: PhysicalKey) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

// ---------------------------------------------------------------------------
// The probe driver: one attached client, one seed pane running the TUI.
// ---------------------------------------------------------------------------

/// A wire-attached probe around one TUI-in-a-pane. Accumulates every
/// `TERMINAL_OUTPUT` chunk twice: rendered through the [`Screen`] oracle
/// (for reaction assertions) and raw (for kitty-activity forensics).
struct TuiProbe {
    stream: UnixStream,
    terminal_id: TerminalId,
    screen: Screen,
    raw: Vec<u8>,
    closed: bool,
}

impl TuiProbe {
    /// Attach to the pre-seeded session, consuming the
    /// `ATTACHED`/`TERMINAL_SNAPSHOT` handshake. The snapshot payload is
    /// REPLAYED into the accumulators, not discarded: per frame.rs, the
    /// snapshot's `vt_replay_bytes` reproduce the grid state at emission,
    /// so any pane output that raced ahead of the attach (e.g. the seed
    /// command's first paint) arrives here and never again as
    /// `TERMINAL_OUTPUT`. Dropping it would blind the `Screen` oracle to
    /// the pane's first paint and make fast-printing scenarios flake.
    async fn attach(mut stream: UnixStream) -> Self {
        send_frame(&mut stream, &attach_by_name("default")).await;
        let (type_byte, attached) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "first frame must be ATTACHED");
        let terminal_id = match attached {
            FrameKind::Attached { snapshot, .. } => {
                assert_eq!(snapshot.panes.len(), 1);
                snapshot.panes[0].id.clone()
            }
            other => panic!("expected ATTACHED, got {other:?}"),
        };
        let (type_byte, snap) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_TERMINAL_SNAPSHOT);
        let mut probe = Self {
            stream,
            terminal_id,
            screen: Screen::new(80, 24).expect("screen oracle"),
            raw: Vec::new(),
            closed: false,
        };
        match snap {
            FrameKind::TerminalSnapshot {
                vt_replay_bytes,
                scrollback_bytes,
                ..
            } => {
                // Scrollback first, per the frame's documented ordering.
                if let Some(scrollback) = scrollback_bytes {
                    probe.raw.extend_from_slice(&scrollback);
                    probe.screen.write(&scrollback);
                }
                probe.raw.extend_from_slice(&vt_replay_bytes);
                probe.screen.write(&vt_replay_bytes);
            }
            other => panic!("expected TERMINAL_SNAPSHOT, got {other:?}"),
        }
        probe
    }

    async fn send_key(&mut self, event: KeyEvent) {
        let frame = FrameKind::InputKey {
            terminal_id: self.terminal_id.clone(),
            event,
        };
        send_frame(&mut self.stream, &frame).await;
    }

    /// Type a printable-ASCII string one keypress at a time.
    async fn type_str(&mut self, s: &str) {
        for c in s.chars() {
            self.send_key(printable_key(c)).await;
        }
    }

    /// Pump one server frame into the accumulators. `false` on EOF /
    /// `TERMINAL_CLOSED` / deadline.
    async fn pump_once(&mut self, deadline: tokio::time::Instant) -> bool {
        if self.closed {
            return false;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let Ok(maybe) = timeout(deadline - now, try_recv_typed(&mut self.stream)).await else {
            return false;
        };
        let Some((type_byte, frame)) = maybe else {
            self.closed = true; // server closed the connection
            return false;
        };
        if type_byte == TYPE_TERMINAL_CLOSED {
            self.closed = true;
            return false;
        }
        if type_byte == TYPE_TERMINAL_OUTPUT
            && let FrameKind::TerminalOutput { bytes, .. } = frame
        {
            self.raw.extend_from_slice(&bytes);
            self.screen.write(&bytes);
        }
        true
    }

    /// Drain output until the rendered screen contains `needle` or the
    /// deadline elapses. Returns whether the needle appeared.
    async fn wait_screen_contains(&mut self, needle: &str) -> bool {
        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        loop {
            if self.screen.contains(needle) {
                return true;
            }
            if !self.pump_once(deadline).await {
                return self.screen.contains(needle);
            }
        }
    }

    /// Assert-flavored wrapper: panics with the rendered screen on miss.
    async fn expect_screen_contains(&mut self, needle: &str, what: &str) {
        assert!(
            self.wait_screen_contains(needle).await,
            "{what}: {needle:?} never appeared on screen.\n--- screen ---\n{}\n--- end ---",
            self.screen.snapshot_text(),
        );
    }

    /// Drain until the pane closes (child exited → `TERMINAL_CLOSED`
    /// and/or server self-exit → EOF). Panics if it never does.
    async fn expect_closed(&mut self, what: &str) {
        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        while self.pump_once(deadline).await {}
        assert!(
            self.closed,
            "{what}: pane never closed.\n--- screen ---\n{}\n--- end ---",
            self.screen.snapshot_text(),
        );
    }

    /// Forensics: did the app push kitty progressive enhancement
    /// (`CSI > … u`) into the pane? This is app→terminal traffic, so it
    /// rides the pane's output stream verbatim. A hit means the per-pane
    /// encoder pivoted to CSI-u for every key sent afterwards.
    fn saw_kitty_push(&self) -> bool {
        contains_csi_u(&self.raw, b'>')
    }

    /// Forensics: did the app *query* kitty support (`CSI ? u`)? Apps
    /// that probe-then-push (nvim) emit this first; libghostty answers on
    /// the PTY input side, which this stream does not carry.
    fn saw_kitty_query(&self) -> bool {
        contains_csi_u(&self.raw, b'?')
    }
}

/// Scan `haystack` for `ESC [ <intro> <digits/;/:> u` — the kitty keyboard
/// push (`intro == b'>'`) or query (`intro == b'?'`) shapes.
fn contains_csi_u(haystack: &[u8], intro: u8) -> bool {
    let mut i = 0;
    while let Some(esc_off) = haystack[i..].iter().position(|&b| b == 0x1b) {
        let seq = &haystack[i + esc_off..];
        if seq.len() >= 3 && seq[1] == b'[' && seq[2] == intro {
            let params = &seq[3..];
            let end = params
                .iter()
                .position(|&b| !(b.is_ascii_digit() || b == b';' || b == b':'));
            if let Some(end) = end
                && params[end] == b'u'
            {
                return true;
            }
        }
        i += esc_off + 1;
    }
    false
}

/// Spawn a server whose seed pane runs `cmd` under `TERM=<term>`, attach,
/// and hand the probe plus the shutdown plumbing to `scenario`.
fn run_tui_probe<F>(cmd: CommandBuilder, term: &str, scenario: F)
where
    F: AsyncFnOnce(&mut TuiProbe),
{
    run_local(async move {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd_and_term(socket_path.clone(), "default", cmd, term);
        let stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        let mut probe = TuiProbe::attach(stream).await;

        scenario(&mut probe).await;

        drop(probe);
        shutdown_tx.send(()).ok();
        let _ = timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, server_handle).await;
    });
}

/// Shell out through `$SHELL`-independent `/bin/sh -c` so pipelines work.
fn sh_c(pipeline: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg(pipeline);
    cmd
}

// ---------------------------------------------------------------------------
// Harness self-check: the seed pane really does see the configured TERM.
// ---------------------------------------------------------------------------

/// Pin the plumbing every ghostty-run below depends on: a seed pane
/// spawned through [`spawn_server_with_seed_cmd_and_term`] must see
/// `TERM=ghostty` in its environment. Without this check a regression in
/// the `ServerConfig::term` → `apply_term` path would silently turn every
/// probe in this file into an xterm-256color control run. No host TUI
/// needed — `/bin/sh` is POSIX-guaranteed — so this one never skips.
#[test]
fn harness_seed_pane_sees_configured_term() {
    let cmd = sh_c("printf 'TERM_IS[%s]' \"$TERM\"; sleep 5");
    run_tui_probe(cmd, "ghostty", async |probe: &mut TuiProbe| {
        probe
            .expect_screen_contains("TERM_IS[ghostty]", "seed pane TERM env")
            .await;
    });
}

// ---------------------------------------------------------------------------
// fzf — type-to-filter + accept. fzf reads keys from /dev/tty (the pane's
// PTY) while its candidate list arrives on stdin.
// ---------------------------------------------------------------------------

async fn fzf_scenario(probe: &mut TuiProbe) {
    // fzf's initial paint: prompt plus the full 3-candidate list.
    probe
        .expect_screen_contains("charlie", "fzf initial list")
        .await;

    // Type-to-filter: the query must echo back AND the match counter
    // must drop to exactly one candidate. Both are round-trips: fzf
    // received each key, reacted, and repainted.
    probe.type_str("brav").await;
    probe
        .expect_screen_contains("> brav", "fzf query echo")
        .await;
    probe
        .expect_screen_contains("1/3", "fzf match counter")
        .await;

    // Accept: fzf prints the selection and exits; the pipeline (and so
    // the pane) exits with it.
    probe.send_key(named_key(PhysicalKey::Enter)).await;
    probe.expect_closed("fzf accept/exit").await;

    eprintln!(
        "kip_roundtrip(fzf): kitty push seen = {}, kitty query seen = {}",
        probe.saw_kitty_push(),
        probe.saw_kitty_query(),
    );
}

/// fzf under `TERM=ghostty`: filter + accept must round-trip.
#[test]
fn fzf_filters_and_accepts_under_term_ghostty() {
    let Some(_fzf) = require_tui("fzf_ghostty", "fzf") else {
        return;
    };
    let cmd = sh_c("printf 'alpha\\nbravo\\ncharlie\\n' | fzf");
    run_tui_probe(cmd, "ghostty", fzf_scenario);
}

/// Control run: the identical fzf scenario under the shipped default
/// `TERM=xterm-256color`. Keeps the harness honest — if this one fails
/// too, the harness (not ghostty) is broken.
#[test]
fn fzf_filters_and_accepts_under_default_xterm() {
    let Some(_fzf) = require_tui("fzf_xterm", "fzf") else {
        return;
    };
    let cmd = sh_c("printf 'alpha\\nbravo\\ncharlie\\n' | fzf");
    run_tui_probe(cmd, "xterm-256color", fzf_scenario);
}

// ---------------------------------------------------------------------------
// less — search prompt, search jump, quit.
// ---------------------------------------------------------------------------

/// less under `TERM=ghostty`: `/137<Enter>` must jump, `q` must quit.
#[test]
fn less_searches_and_quits_under_term_ghostty() {
    let Some(_less) = require_tui("less_ghostty", "less") else {
        return;
    };
    // 200 numbered lines from a temp file; `-c` clears per repaint so the
    // search jump fully redraws row 0.
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("numbers.txt");
    let body: String = (1..=200).fold(String::new(), |mut acc, n| {
        use std::fmt::Write as _;
        let _ = writeln!(acc, "line-{n}");
        acc
    });
    std::fs::write(&file, body).unwrap();
    let mut cmd = CommandBuilder::new("less");
    cmd.arg(file.to_str().unwrap());

    run_tui_probe(cmd, "ghostty", async |probe: &mut TuiProbe| {
        probe
            .expect_screen_contains("line-1", "less initial page")
            .await;

        // `/` opens the search prompt; the typed pattern must echo.
        probe.send_key(printable_key('/')).await;
        probe.type_str("line-137").await;
        probe
            .expect_screen_contains("/line-137", "less search prompt echo")
            .await;

        // Enter executes the search: the match scrolls to the top row.
        probe.send_key(named_key(PhysicalKey::Enter)).await;
        probe
            .expect_screen_contains("line-137", "less search jump")
            .await;

        // q quits less; the pane closes.
        probe.send_key(printable_key('q')).await;
        probe.expect_closed("less quit").await;

        eprintln!(
            "kip_roundtrip(less): kitty push seen = {}, kitty query seen = {}",
            probe.saw_kitty_push(),
            probe.saw_kitty_query(),
        );
    });
}

// ---------------------------------------------------------------------------
// nvim — the KIP app: modern nvim probes for and enables the kitty
// keyboard protocol when the terminal supports it, so under TERM=ghostty
// every key below goes over the wire, pivots to CSI-u in the per-pane
// encoder, and must be parsed back by nvim. This is the genuine
// round-trip the bead asks for.
// ---------------------------------------------------------------------------

/// nvim under `TERM=ghostty`: insert-mode text entry and `:q!` must
/// round-trip even after nvim enables the kitty keyboard protocol.
#[test]
fn nvim_kip_insert_and_quit_under_term_ghostty() {
    let Some(_nvim) = require_tui("nvim_ghostty", "nvim") else {
        return;
    };
    let mut cmd = CommandBuilder::new("nvim");
    cmd.arg("--clean");

    run_tui_probe(cmd, "ghostty", async |probe: &mut TuiProbe| {
        // `--clean` still draws the default statusline: "[No Name]".
        probe
            .expect_screen_contains("[No Name]", "nvim startup")
            .await;

        // Enter insert mode and type a sentinel.
        probe.send_key(printable_key('i')).await;
        probe.type_str("kip roundtrip ok").await;
        probe
            .expect_screen_contains("kip roundtrip ok", "nvim insert-mode echo")
            .await;

        // Esc back to normal mode, then :q! to quit without saving.
        probe.send_key(named_key(PhysicalKey::Escape)).await;
        probe.type_str(":q!").await;
        probe
            .expect_screen_contains(":q!", "nvim cmdline echo")
            .await;
        probe.send_key(named_key(PhysicalKey::Enter)).await;
        probe.expect_closed("nvim :q!").await;

        // Forensics: report whether nvim actually engaged KIP. The
        // reaction assertions above prove the round-trip either way; this
        // line records whether the CSI-u pivot was exercised.
        eprintln!(
            "kip_roundtrip(nvim): kitty push seen = {}, kitty query seen = {}",
            probe.saw_kitty_push(),
            probe.saw_kitty_query(),
        );
    });
}

// ---------------------------------------------------------------------------
// vim — classic vim (9.x): no kitty support; under TERM=ghostty it must
// keep working in legacy/modifyOtherKeys mode.
// ---------------------------------------------------------------------------

/// vim under `TERM=ghostty`: insert-mode text entry and `:q!` must work.
#[test]
fn vim_insert_and_quit_under_term_ghostty() {
    let Some(_vim) = require_tui("vim_ghostty", "vim") else {
        return;
    };
    let mut cmd = CommandBuilder::new("vim");
    cmd.arg("-u");
    cmd.arg("NONE");
    cmd.arg("-i");
    cmd.arg("NONE");

    run_tui_probe(cmd, "ghostty", async |probe: &mut TuiProbe| {
        // The vanilla vim intro screen names itself.
        probe
            .expect_screen_contains("VIM - Vi IMproved", "vim startup")
            .await;

        probe.send_key(printable_key('i')).await;
        probe.type_str("kip roundtrip ok").await;
        probe
            .expect_screen_contains("kip roundtrip ok", "vim insert-mode echo")
            .await;

        probe.send_key(named_key(PhysicalKey::Escape)).await;
        probe.type_str(":q!").await;
        probe
            .expect_screen_contains(":q!", "vim cmdline echo")
            .await;
        probe.send_key(named_key(PhysicalKey::Enter)).await;
        probe.expect_closed("vim :q!").await;

        eprintln!(
            "kip_roundtrip(vim): kitty push seen = {}, kitty query seen = {}",
            probe.saw_kitty_push(),
            probe.saw_kitty_query(),
        );
    });
}

// ---------------------------------------------------------------------------
// btop — a heavy non-ncurses TUI with its own input parser.
// ---------------------------------------------------------------------------

/// btop under `TERM=ghostty`: the dashboard must render and `q` must quit.
#[test]
fn btop_quits_on_q_under_term_ghostty() {
    let Some(_btop) = require_tui("btop_ghostty", "btop") else {
        return;
    };
    let mut cmd = CommandBuilder::new("btop");
    // btop refuses to start without a UTF-8 locale; the nextest
    // environment does not guarantee one.
    cmd.env("LANG", "en_US.UTF-8");
    cmd.env("LC_ALL", "en_US.UTF-8");

    run_tui_probe(cmd, "ghostty", async |probe: &mut TuiProbe| {
        // The default theme titles its boxes in lowercase ("cpu", "mem",
        // "net", "proc").
        probe
            .expect_screen_contains("cpu", "btop dashboard render")
            .await;

        probe.send_key(printable_key('q')).await;
        probe.expect_closed("btop quit").await;

        eprintln!(
            "kip_roundtrip(btop): kitty push seen = {}, kitty query seen = {}",
            probe.saw_kitty_push(),
            probe.saw_kitty_query(),
        );
    });
}

// ---------------------------------------------------------------------------
// htop — the canonical phux-7vx reproducer. NOT installed on the machine
// this harness was built on and not nix-pinned, so this probe is ignored
// rather than probe-skipped: running it must be a deliberate act
// (`cargo nextest run --run-ignored all -E 'test(htop_quits)'`).
//
// Both outcomes are informative:
// - PASS: htop (this build of it) round-trips its keys under
//   TERM=ghostty — evidence toward restoring the ghostty default.
// - FAIL at `expect_closed`: the phux-7vx gap reproduced end-to-end —
//   htop pushed kitty flags it cannot parse back, `q` was CSI-u-encoded,
//   and the quit never fired. Keep TERM=xterm-256color.
// ---------------------------------------------------------------------------

/// htop under `TERM=ghostty`: renders, then `q` must quit (phux-7vx).
#[test]
#[ignore = "htop is host-provided and was absent when phux-0o8 ran; run deliberately on a host with htop"]
fn htop_quits_on_q_under_term_ghostty() {
    let Some(_htop) = require_tui("htop_ghostty", "htop") else {
        return;
    };
    let cmd = CommandBuilder::new("htop");

    run_tui_probe(cmd, "ghostty", async |probe: &mut TuiProbe| {
        // htop's header always includes the load average label.
        probe
            .expect_screen_contains("Load average", "htop render")
            .await;

        probe.send_key(printable_key('q')).await;
        probe.expect_closed("htop quit (phux-7vx shape)").await;

        eprintln!(
            "kip_roundtrip(htop): kitty push seen = {}, kitty query seen = {}",
            probe.saw_kitty_push(),
            probe.saw_kitty_query(),
        );
    });
}
