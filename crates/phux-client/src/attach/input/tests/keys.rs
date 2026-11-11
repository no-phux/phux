//! Legacy and extended keyboard decoding and native encoder round trips.
use super::*;

#[test]
fn key_sequences_decode_to_key_and_mods() {
    let shift_alt_ctrl = ModSet::SHIFT | ModSet::ALT | ModSet::CTRL;
    let cases: &[(&[u8], PhysicalKey, ModSet)] = &[
        (b"a", PhysicalKey::A, ModSet::empty()),
        (b"A", PhysicalKey::A, ModSet::SHIFT),
        (b"-", PhysicalKey::Minus, ModSet::empty()),
        (b"|", PhysicalKey::Backslash, ModSet::SHIFT),
        (b"\r", PhysicalKey::Enter, ModSet::empty()),
        (b"\t", PhysicalKey::Tab, ModSet::empty()),
        (b"\x7f", PhysicalKey::Backspace, ModSet::empty()),
        (b"\x03", PhysicalKey::C, ModSet::CTRL),
        (b"\x02", PhysicalKey::B, ModSet::CTRL),
        // LF is Ctrl-J, not Enter: the encoder would otherwise send CR.
        (b"\n", PhysicalKey::J, ModSet::CTRL),
        (b"\x1ba", PhysicalKey::A, ModSet::ALT),
        (b"\x1b[A", PhysicalKey::ArrowUp, ModSet::empty()),
        (b"\x1b[1;5A", PhysicalKey::ArrowUp, ModSet::CTRL),
        (b"\x1b[1;8C", PhysicalKey::ArrowRight, shift_alt_ctrl),
        (b"\x1b[1;9D", PhysicalKey::ArrowLeft, ModSet::SUPER),
        // A private marker is not differentiated; a first param other
        // than 1 carries no modifier.
        (b"\x1b[>1;5A", PhysicalKey::ArrowUp, ModSet::CTRL),
        (b"\x1b[2;5A", PhysicalKey::ArrowUp, ModSet::empty()),
        (b"\x1b[H", PhysicalKey::Home, ModSet::empty()),
        (b"\x1b[1~", PhysicalKey::Home, ModSet::empty()),
        (b"\x1b[5~", PhysicalKey::PageUp, ModSet::empty()),
        (b"\x1b[5;5~", PhysicalKey::PageUp, ModSet::CTRL),
        (b"\x1b[15~", PhysicalKey::F5, ModSet::empty()),
        (b"\x1b[17~", PhysicalKey::F6, ModSet::empty()),
        (b"\x1b[18~", PhysicalKey::F7, ModSet::empty()),
        (b"\x1b[19~", PhysicalKey::F8, ModSet::empty()),
        (b"\x1b[20~", PhysicalKey::F9, ModSet::empty()),
        (b"\x1b[21~", PhysicalKey::F10, ModSet::empty()),
        (b"\x1b[23~", PhysicalKey::F11, ModSet::empty()),
        (b"\x1b[24~", PhysicalKey::F12, ModSet::empty()),
        // SS3, including `ESC O P`, which must not read as a focus report.
        (b"\x1bOP", PhysicalKey::F1, ModSet::empty()),
        (b"\x1bOQ", PhysicalKey::F2, ModSet::empty()),
        (b"\x1bOR", PhysicalKey::F3, ModSet::empty()),
        (b"\x1bOS", PhysicalKey::F4, ModSet::empty()),
        (b"\x1bOA", PhysicalKey::ArrowUp, ModSet::empty()),
        // Kitty `CSI u`.
        (b"\x1b[97u", PhysicalKey::A, ModSet::empty()),
        (b"\x1b[97;5u", PhysicalKey::A, ModSet::CTRL),
        (b"\x1b[97;8u", PhysicalKey::A, shift_alt_ctrl),
        (b"\x1b[97;9u", PhysicalKey::A, ModSet::SUPER),
        (b"\x1b[97;17u", PhysicalKey::A, ModSet::SUPER),
        (b"\x1b[97;33u", PhysicalKey::A, ModSet::ALT),
        (b"\x1b[97;65u", PhysicalKey::A, ModSet::CAPS_LOCK),
        (b"\x1b[97;129u", PhysicalKey::A, ModSet::NUM_LOCK),
        (b"\x1b[97:65:97;2u", PhysicalKey::A, ModSet::SHIFT),
        (b"\x1b[57368u", PhysicalKey::F5, ModSet::empty()),
        (b"\x1b[57352u", PhysicalKey::ArrowUp, ModSet::empty()),
        (b"\x1b[27u", PhysicalKey::Escape, ModSet::empty()),
        (b"\x1b[13u", PhysicalKey::Enter, ModSet::empty()),
    ];
    for &(input, key, mods) in cases {
        let ev = one_key(input);
        assert_eq!((ev.key, ev.mods), (key, mods), "input {input:?}");
    }
}

/// `CSI Z` is Shift+Tab (Claude Code's mode toggle); dropping the Shift
/// delivered a plain Tab.
#[test]
fn csi_z_is_shift_tab() {
    let ev = one_key(b"\x1b[Z");
    assert_eq!((ev.key, ev.mods), (PhysicalKey::Tab, ModSet::SHIFT));
    let ev = one_key(b"\x1b[1;5Z");
    assert_eq!(
        (ev.key, ev.mods),
        (PhysicalKey::Tab, ModSet::SHIFT | ModSet::CTRL)
    );
}

/// Encode `ev` the way the server would for a pane with `flags`, which is
/// also what native Ghostty sends for the same key.
fn encode(ev: &KeyEvent, flags: libghostty_vt::key::KittyKeyFlags) -> Vec<u8> {
    use libghostty_vt::key::{Encoder, EncoderOptions, Event};
    let term = libghostty_vt::Terminal::new(80, 24).expect("terminal");
    let mut opts = EncoderOptions::from_terminal(&term).expect("options");
    opts.kitty_flags = flags;
    let mut enc = Encoder::new().expect("encoder");
    enc.set_options(opts);
    let mut lg = Event::new().expect("event");
    lg.set_action(ev.action.into())
        .set_key(ev.key.into())
        .set_mods(ev.mods.into())
        .set_consumed_mods(ev.consumed_mods.into())
        .set_utf8(ev.text.clone());
    if let Some(c) = ev.unshifted_codepoint.and_then(char::from_u32) {
        lg.set_unshifted_codepoint(c);
    }
    let mut out = Vec::new();
    enc.encode_to_vec(&lg, &mut out).expect("encode");
    out
}

/// Under the host kitty flags phux pushes (disambiguate), a key typed in
/// the host round-trips: the host encodes it, phux decodes it, and the
/// server re-encodes it to exactly what native Ghostty would send a pane
/// with or without kitty flags.
#[test]
fn host_kitty_disambiguate_round_trips_like_native_ghostty() {
    use libghostty_vt::key::KittyKeyFlags;
    let (s, c, a, m) = (ModSet::SHIFT, ModSet::CTRL, ModSet::ALT, ModSet::SUPER);
    let key = |key, mods, cp: Option<char>| KeyEvent {
        unshifted_codepoint: cp.map(u32::from),
        ..make_named_key(key, mods)
    };
    let text = |k, mods: ModSet, cp: char, t: &str| KeyEvent {
        consumed_mods: mods,
        text: Some(t.to_owned()),
        ..key(k, mods, Some(cp))
    };
    let cases = [
        key(PhysicalKey::Enter, m, None),
        key(PhysicalKey::Enter, s, None),
        key(PhysicalKey::Enter, a, None),
        key(PhysicalKey::Enter, c, None),
        key(PhysicalKey::Tab, s, None),
        key(PhysicalKey::Tab, c, None),
        key(PhysicalKey::I, c, Some('i')),
        key(PhysicalKey::H, c, Some('h')),
        key(PhysicalKey::Backspace, c, None),
        key(PhysicalKey::Backspace, a, None),
        key(PhysicalKey::Escape, ModSet::empty(), None),
        key(PhysicalKey::A, c, Some('a')),
        key(PhysicalKey::A, c | s, Some('a')),
        key(PhysicalKey::A, a, Some('a')),
        key(PhysicalKey::A, c | a, Some('a')),
        key(PhysicalKey::A, m, Some('a')),
        key(PhysicalKey::BracketRight, c, Some(']')),
        key(PhysicalKey::Slash, c, Some('/')),
        key(PhysicalKey::Backslash, c, Some('\\')),
        key(PhysicalKey::Digit2, c, Some('2')),
        key(PhysicalKey::Space, c, Some(' ')),
        key(PhysicalKey::ArrowLeft, c | s, None),
        key(PhysicalKey::Delete, a, None),
        key(PhysicalKey::F5, c, None),
        text(PhysicalKey::A, ModSet::empty(), 'a', "a"),
        text(PhysicalKey::A, s, 'a', "A"),
        text(PhysicalKey::Slash, s, '/', "?"),
        text(PhysicalKey::Numpad1, ModSet::empty(), '1', "1"),
    ];
    for native in cases {
        let host = encode(&native, KittyKeyFlags::DISAMBIGUATE);
        let decoded = one_key(&host);
        for pane in [KittyKeyFlags::DISABLED, KittyKeyFlags::DISAMBIGUATE] {
            assert_eq!(
                encode(&decoded, pane),
                encode(&native, pane),
                "{:?}+{:?} via host {host:?}, pane {pane:?}",
                native.mods,
                native.key
            );
        }
    }
}

/// Ctrl+Backspace arrives as BS (0x08) and must keep its Ctrl, so a pane
/// under kitty flags gets `CSI 127;5u` (word delete) instead of a plain
/// Backspace; a legacy pane still gets 0x08.
#[test]
fn bs_is_ctrl_backspace() {
    use libghostty_vt::key::KittyKeyFlags;
    let ev = one_key(b"\x08");
    assert_eq!((ev.key, ev.mods), (PhysicalKey::Backspace, ModSet::CTRL));
    assert_eq!(encode(&ev, KittyKeyFlags::DISAMBIGUATE), b"\x1b[127;5u");
    assert_eq!(encode(&ev, KittyKeyFlags::DISABLED), b"\x08");
    let ev = one_key(b"\x7f");
    assert_eq!((ev.key, ev.mods), (PhysicalKey::Backspace, ModSet::empty()));
}

/// Host Ghostty in legacy mode sends modified Enter as xterm
/// modifyOtherKeys `CSI 27;mod;13~` (Cmd+Return is `CSI 27;9;13~`). It
/// must decode to Enter+mods so the pane's encoder re-emits exactly what
/// Ghostty would natively: `CSI 13;9u` under kitty flags (Claude Code's
/// newline), the same `CSI 27;9;13~` otherwise.
#[test]
fn modify_other_keys_enter_round_trips_like_native_ghostty() {
    use libghostty_vt::key::KittyKeyFlags;
    for (wire, mods, kitty) in [
        (&b"\x1b[27;9;13~"[..], ModSet::SUPER, &b"\x1b[13;9u"[..]),
        (b"\x1b[27;2;13~", ModSet::SHIFT, b"\x1b[13;2u"),
        (b"\x1b[27;5;13~", ModSet::CTRL, b"\x1b[13;5u"),
    ] {
        let ev = one_key(wire);
        assert_eq!((ev.key, ev.mods), (PhysicalKey::Enter, mods), "{wire:?}");
        assert_eq!(encode(&ev, KittyKeyFlags::DISAMBIGUATE), kitty, "{wire:?}");
        assert_eq!(encode(&ev, KittyKeyFlags::DISABLED), wire, "{wire:?}");
    }
}

#[test]
fn printable_keys_carry_text_and_consumed_shift() {
    let lower = one_key(b"a");
    assert_eq!(lower.text.as_deref(), Some("a"));
    assert_eq!(lower.action, KeyAction::Press);
    let upper = one_key(b"A");
    assert!(upper.consumed_mods.contains(ModSet::SHIFT));
    assert_eq!(upper.unshifted_codepoint, Some(u32::from('a')));
    assert_eq!(one_key(b"\x1ba").text, None, "Alt-letter carries no text");
}

/// Punctuation must decode to the same (key, shift) the chord parser
/// builds for the glyph, or default binds like `"|"` never fire.
#[test]
fn punctuation_matches_chord_parser() {
    for &glyph in b"|-`=[]\\;',./~!@#$%^&*()_+{}:\"<>?" {
        let ev = one_key(&[glyph]);
        let chord =
            phux_config::keybind::parse_chord(&char::from(glyph).to_string()).expect("chord parse");
        assert_eq!(
            (ev.key, ev.mods.contains(ModSet::SHIFT)),
            (chord.key, chord.modifiers.contains(ModSet::SHIFT)),
            "mismatch on glyph {}",
            char::from(glyph)
        );
    }
}

#[test]
fn utf8_decodes_and_recovers_from_a_bad_continuation() {
    let e_acute = one_key(&[0xC3, 0xA9]);
    assert_eq!(e_acute.text.as_deref(), Some("é"));
    assert_eq!(e_acute.unshifted_codepoint, Some(0x00E9));
    assert_eq!(one_key("😀".as_bytes()).text.as_deref(), Some("😀"));
    // A lead byte followed by ASCII drops the lead and keeps the ASCII.
    assert_eq!(one_key(&[0xC3, b'a']).text.as_deref(), Some("a"));
}

#[test]
fn kitty_event_types_and_associated_text() {
    let actions: &[(&[u8], KeyAction)] = &[
        (b"\x1b[97;1:1u", KeyAction::Press),
        (b"\x1b[97;1:2u", KeyAction::Repeat),
        (b"\x1b[97;1:3u", KeyAction::Release),
    ];
    for &(input, action) in actions {
        assert_eq!(one_key(input).action, action, "input {input:?}");
    }
    let texts: &[(&[u8], Option<&str>)] = &[
        (b"\x1b[97;1:1:97u", Some("a")),
        (b"\x1b[97;1:1:97:769u", Some("a\u{0301}")),
        (b"\x1b[97;1:1:0u", None),
        (b"\x1b[57364;1:1:120u", Some("x")),
    ];
    for &(input, text) in texts {
        assert_eq!(one_key(input).text.as_deref(), text, "input {input:?}");
    }
    assert_eq!(one_key(b"\x1b[97u").unshifted_codepoint, Some(97));
}

#[test]
fn legacy_ctrl_bracket_right_is_not_dropped() {
    let key = one_key(b"\x1d");
    assert_eq!(key.key, PhysicalKey::BracketRight);
    assert_eq!(key.mods, ModSet::CTRL);
    assert_eq!(key.action, KeyAction::Press);
    assert_eq!(
        encode(&key, libghostty_vt::key::KittyKeyFlags::DISABLED),
        b"\x1d"
    );
}
