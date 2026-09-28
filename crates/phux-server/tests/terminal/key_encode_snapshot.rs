//! Byte-exact contract for the server-side key encoder across the legacy,
//! modifyOtherKeys, and kitty keyboard protocol modes (docs/spec/input.md §2.6).
//! Any change in the PTY bytes a (key, mods, terminal mode) triple produces
//! must show up here as a diff.

use libghostty_vt::Terminal as GhosttyTerminal;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_server::input::key::PerTerminalKeyEncoder;

struct Case {
    name: &'static str,
    /// VT written to the terminal before encoding (mode setup).
    setup: &'static [u8],
    action: KeyAction,
    key: PhysicalKey,
    mods: ModSet,
    composing: bool,
    text: Option<&'static str>,
    codepoint: Option<char>,
    expected: &'static [u8],
}

const fn press(
    name: &'static str,
    key: PhysicalKey,
    mods: ModSet,
    expected: &'static [u8],
) -> Case {
    Case {
        name,
        setup: b"",
        action: KeyAction::Press,
        key,
        mods,
        composing: false,
        text: None,
        codepoint: None,
        expected,
    }
}

#[test]
fn key_encoder_bytes_per_mode() {
    let modify_other_keys_2: &[u8] = b"\x1b[>4;2m";
    let cases = [
        Case {
            text: Some("a"),
            codepoint: Some('a'),
            ..press("plain a", PhysicalKey::A, ModSet::empty(), b"a")
        },
        Case {
            codepoint: Some('c'),
            ..press("ctrl-c", PhysicalKey::C, ModSet::CTRL, b"\x03")
        },
        press("shift-tab", PhysicalKey::Tab, ModSet::SHIFT, b"\x1b[Z"),
        press("alt-enter", PhysicalKey::Enter, ModSet::ALT, b"\x1b\r"),
        press("esc", PhysicalKey::Escape, ModSet::empty(), b"\x1b"),
        press(
            "up, kip off",
            PhysicalKey::ArrowUp,
            ModSet::empty(),
            b"\x1b[A",
        ),
        // IME composition suppresses PTY output entirely.
        Case {
            composing: true,
            codepoint: Some('\''),
            ..press("dead key", PhysicalKey::Quote, ModSet::empty(), b"")
        },
        Case {
            setup: modify_other_keys_2,
            ..press(
                "f1, modifyOtherKeys=2",
                PhysicalKey::F1,
                ModSet::empty(),
                b"\x1bOP",
            )
        },
        Case {
            setup: modify_other_keys_2,
            ..press(
                "f12, modifyOtherKeys=2",
                PhysicalKey::F12,
                ModSet::empty(),
                b"\x1b[24~",
            )
        },
        // 31 = every kitty keyboard flag.
        Case {
            setup: b"\x1b[>31u",
            ..press(
                "up, kip all",
                PhysicalKey::ArrowUp,
                ModSet::empty(),
                b"\x1b[1;1:1A",
            )
        },
        // 3 = DISAMBIGUATE | REPORT_EVENTS: releases become visible.
        Case {
            setup: b"\x1b[>3u",
            action: KeyAction::Release,
            codepoint: Some('a'),
            ..press(
                "release a, kip events",
                PhysicalKey::A,
                ModSet::empty(),
                b"\x1b[97;1:3u",
            )
        },
    ];

    for case in cases {
        let mut terminal = GhosttyTerminal::new(80, 24).expect("Terminal::new");
        terminal.vt_write(case.setup);
        let event = KeyEvent {
            action: case.action,
            key: case.key,
            mods: case.mods,
            consumed_mods: ModSet::empty(),
            composing: case.composing,
            text: case.text.map(str::to_owned),
            unshifted_codepoint: case.codepoint.map(u32::from),
        };
        let mut encoder = PerTerminalKeyEncoder::new().expect("encoder");
        let bytes = encoder.encode(&event, &terminal).expect("encode");
        assert_eq!(
            bytes.escape_ascii().to_string(),
            case.expected.escape_ascii().to_string(),
            "{}",
            case.name
        );
    }
}
