//! Snapshot tests for the server-side key encoder (phux-6yl.3).
//!
//! Each test builds a representative `KeyEvent`, runs it through
//! [`PerTerminalKeyEncoder`], hex-dumps the produced PTY bytes, and asserts
//! against a committed insta snapshot under `tests/snapshots/`. The key
//! encoding contract is a cross-implementation interface: any change in
//! what bytes a given (key, mods, terminal state) triple produces MUST
//! surface as a visible diff in PR review.
//!
//! Coverage spans the KIP / fixterms / modifyOtherKeys matrix at the
//! encoder boundary. See docs/spec/input.md §2.6.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use libghostty_vt::Terminal as GhosttyTerminal;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_server::input::key::PerTerminalKeyEncoder;

/// Render `bytes` in a `xxd`-style hex dump: 16 columns per row, formatted as
/// `OFFSET | HEX HEX HEX ... | ASCII`. Mirrors the style used by
/// `crates/phux-protocol/tests/diff_wire_snapshots.rs` so snapshots across
/// the workspace read the same way.
fn hex_dump(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    if bytes.is_empty() {
        out.push_str("(empty)\n");
        return out;
    }

    for (chunk_idx, chunk) in bytes.chunks(16).enumerate() {
        let offset = chunk_idx * 16;
        let _ = write!(out, "{offset:08x} |");
        for (i, b) in chunk.iter().enumerate() {
            if i == 8 {
                out.push(' ');
            }
            let _ = write!(out, " {b:02x}");
        }
        // Pad short last row so the ASCII column lines up.
        let pad_cells = 16 - chunk.len();
        for i in 0..pad_cells {
            if chunk.len() + i == 8 {
                out.push(' ');
            }
            out.push_str("   ");
        }
        out.push_str(" |");
        for b in chunk {
            let c = if (0x20..=0x7e).contains(b) {
                *b as char
            } else {
                '.'
            };
            out.push(c);
        }
        out.push('\n');
    }
    out
}

/// Fresh 80x24 terminal with no extra modes set.
fn make_terminal() -> GhosttyTerminal<'static, 'static> {
    {
        let mut terminal = GhosttyTerminal::new(80, 24).expect("Terminal::new");
        terminal
            .set_scrollback_max_lines(Some(1000))
            .expect("Terminal::new");
        terminal
    }
}

/// Enable xterm modifyOtherKeys=2 (`CSI > 4 ; 2 m`) on a terminal.
fn enable_modify_other_keys_2(t: &mut GhosttyTerminal<'_, '_>) {
    t.vt_write(b"\x1b[>4;2m");
}

/// Push KIP flags onto the terminal's keyboard-protocol stack
/// (`CSI > <flags> u`). `flags` is the bitfield documented in the kitty
/// keyboard protocol spec; the encoder reads it via
/// `Terminal::kitty_keyboard_flags()` and `set_options_from_terminal`.
fn push_kitty_flags(t: &mut GhosttyTerminal<'_, '_>, flags: u8) {
    let seq = format!("\x1b[>{flags}u");
    t.vt_write(seq.as_bytes());
}

/// Encode one event with a fresh per-pane encoder and return the hex dump.
fn dump_encode(event: &KeyEvent, terminal: &GhosttyTerminal<'_, '_>) -> String {
    let mut enc = PerTerminalKeyEncoder::new().expect("encoder");
    let bytes = enc.encode(event, terminal).expect("encode");
    hex_dump(bytes)
}

// --------------------------------------------------------------------
// KIP level: DISABLED (no flags pushed, no modifyOtherKeys)
// --------------------------------------------------------------------

#[test]
fn snap_plain_ascii_a_press() {
    let terminal = make_terminal();
    let ev = KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::A,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: Some("a".to_owned()),
        unshifted_codepoint: Some(u32::from('a')),
    };
    insta::assert_snapshot!(dump_encode(&ev, &terminal));
}

#[test]
fn snap_ctrl_c() {
    let terminal = make_terminal();
    let ev = KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::C,
        mods: ModSet::CTRL,
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: Some(u32::from('c')),
    };
    insta::assert_snapshot!(dump_encode(&ev, &terminal));
}

#[test]
fn snap_shift_tab() {
    let terminal = make_terminal();
    let ev = KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::Tab,
        mods: ModSet::SHIFT,
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    };
    insta::assert_snapshot!(dump_encode(&ev, &terminal));
}

#[test]
fn snap_alt_enter() {
    let terminal = make_terminal();
    let ev = KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::Enter,
        mods: ModSet::ALT,
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    };
    insta::assert_snapshot!(dump_encode(&ev, &terminal));
}

#[test]
fn snap_esc() {
    let terminal = make_terminal();
    let ev = KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::Escape,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    };
    insta::assert_snapshot!(dump_encode(&ev, &terminal));
}

#[test]
fn snap_up_arrow_kip_disabled() {
    let terminal = make_terminal();
    let ev = KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::ArrowUp,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    };
    insta::assert_snapshot!(dump_encode(&ev, &terminal));
}

#[test]
fn snap_composing_dead_key() {
    // While IME composition is active, the encoder should suppress PTY
    // output — the inner program shouldn't see the half-formed key.
    let terminal = make_terminal();
    let ev = KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::Quote,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: true,
        text: None,
        unshifted_codepoint: Some(u32::from('\'')),
    };
    insta::assert_snapshot!(dump_encode(&ev, &terminal));
}

// --------------------------------------------------------------------
// KIP level: modifyOtherKeys = 2 (xterm fixterms extension)
// --------------------------------------------------------------------

#[test]
fn snap_f1_modify_other_keys_2() {
    let mut terminal = make_terminal();
    enable_modify_other_keys_2(&mut terminal);
    let ev = KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::F1,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    };
    insta::assert_snapshot!(dump_encode(&ev, &terminal));
}

#[test]
fn snap_f12_modify_other_keys_2() {
    let mut terminal = make_terminal();
    enable_modify_other_keys_2(&mut terminal);
    let ev = KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::F12,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    };
    insta::assert_snapshot!(dump_encode(&ev, &terminal));
}

// --------------------------------------------------------------------
// KIP level: REPORT_ALL (all kitty keyboard flags on)
// --------------------------------------------------------------------

#[test]
fn snap_up_arrow_kip_report_all_keys() {
    let mut terminal = make_terminal();
    // Flags bit value 31 = DISAMBIGUATE | REPORT_EVENTS | REPORT_ALTERNATES
    // | REPORT_ALL | REPORT_ASSOCIATED — i.e. everything (KittyKeyFlags::ALL).
    push_kitty_flags(&mut terminal, 31);
    let ev = KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::ArrowUp,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    };
    insta::assert_snapshot!(dump_encode(&ev, &terminal));
}

// --------------------------------------------------------------------
// KIP level: REPORT_EVENTS (release events should now produce bytes)
// --------------------------------------------------------------------

#[test]
fn snap_release_kip_report_event_types() {
    let mut terminal = make_terminal();
    // 3 = DISAMBIGUATE | REPORT_EVENTS — the minimum to make release
    // events visible to the inner program.
    push_kitty_flags(&mut terminal, 3);
    let ev = KeyEvent {
        action: KeyAction::Release,
        key: PhysicalKey::A,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: Some(u32::from('a')),
    };
    insta::assert_snapshot!(dump_encode(&ev, &terminal));
}
