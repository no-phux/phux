//! Key identities, legacy controls, modifiers, and UTF-8 byte classification.
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

/// Map a single byte to a [`KeyEvent`] for the printable / C0 region.
/// Returns `None` for bytes the parser handles elsewhere (ESC, UTF-8
/// continuations, ...).
pub(super) fn c0_or_ascii_to_key(b: u8) -> Option<KeyEvent> {
    match b {
        // Printable ASCII.
        0x20..=0x7E => Some(KeyEvent {
            action: KeyAction::Press,
            key: ascii_to_physical(b),
            mods: ascii_shift_mods(b),
            consumed_mods: ascii_shift_mods(b),
            composing: false,
            text: Some(char::from(b).to_string()),
            unshifted_codepoint: Some(u32::from(ascii_unshifted(b))),
        }),
        // CR → Enter. LF (0x0A) is Ctrl+J: it falls through to the Ctrl-letter
        // arm below so the server encoder reproduces 0x0A rather than CR. In raw
        // mode the host TTY sends CR for Return and LF only for Ctrl+J, so this
        // split is unambiguous.
        0x0D => Some(make_named_key(PhysicalKey::Enter, ModSet::empty())),
        // DEL → Backspace. BS is what Ghostty (and xterm) send for
        // Ctrl+Backspace; it is also legacy Ctrl+H, which the two share.
        0x7F => Some(make_named_key(PhysicalKey::Backspace, ModSet::empty())),
        0x08 => Some(make_named_key(PhysicalKey::Backspace, ModSet::CTRL)),
        // HT → Tab.
        0x09 => Some(make_named_key(PhysicalKey::Tab, ModSet::empty())),
        // Legacy Ctrl-] (the native-client detach chord). Kitty hosts send
        // CSI 93;5u, but ordinary VT hosts send this C0 byte.
        0x1D => Some(make_named_key(PhysicalKey::BracketRight, ModSet::CTRL)),
        // Ctrl-A..Ctrl-Z (skipping the dedicated mappings above and ESC). LF
        // (0x0A) lands here as Ctrl+J → letter 'J'.
        0x01..=0x1A if b != 0x08 && b != 0x09 && b != 0x0D => {
            let letter = b'A' + (b - 1);
            Some(KeyEvent {
                action: KeyAction::Press,
                key: letter_key(letter),
                mods: ModSet::CTRL,
                consumed_mods: ModSet::CTRL,
                composing: false,
                text: None,
                unshifted_codepoint: Some(u32::from(letter.to_ascii_lowercase())),
            })
        }
        _ => None,
    }
}

/// Build a key event for a "named" key (no text payload).
#[must_use]
pub const fn make_named_key(key: PhysicalKey, mods: ModSet) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods,
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

/// Map a printable ASCII byte to a [`PhysicalKey`]. Punctuation goes through
/// [`phux_config::keybind::punct_to_key`] so it matches the key the chord
/// parser builds for the same glyph, or punctuation keybinds never fire.
const fn ascii_to_physical(b: u8) -> PhysicalKey {
    match b {
        b' ' => PhysicalKey::Space,
        b'0'..=b'9' => match b {
            b'0' => PhysicalKey::Digit0,
            b'1' => PhysicalKey::Digit1,
            b'2' => PhysicalKey::Digit2,
            b'3' => PhysicalKey::Digit3,
            b'4' => PhysicalKey::Digit4,
            b'5' => PhysicalKey::Digit5,
            b'6' => PhysicalKey::Digit6,
            b'7' => PhysicalKey::Digit7,
            b'8' => PhysicalKey::Digit8,
            _ => PhysicalKey::Digit9,
        },
        b'A'..=b'Z' | b'a'..=b'z' => letter_key(b.to_ascii_uppercase()),
        _ => match phux_config::keybind::punct_to_key(b as char) {
            Some((key, _shift)) => key,
            None => PhysicalKey::Unidentified,
        },
    }
}

/// `SHIFT` for uppercase letters and shifted punctuation, which also goes in
/// `consumed_mods` (SPEC §9.1.3) so the encoder does not apply it twice.
const fn ascii_shift_mods(b: u8) -> ModSet {
    if ascii_unshifted(b) == b {
        ModSet::empty()
    } else {
        ModSet::SHIFT
    }
}

/// The unmodified glyph for `unshifted_codepoint` (`A` -> `a`, `@` -> `2`),
/// assuming US QWERTY.
const fn ascii_unshifted(b: u8) -> u8 {
    match b {
        b'A'..=b'Z' => b + 32,
        b'!' => b'1',
        b'@' => b'2',
        b'#' => b'3',
        b'$' => b'4',
        b'%' => b'5',
        b'^' => b'6',
        b'&' => b'7',
        b'*' => b'8',
        b'(' => b'9',
        b')' => b'0',
        b'_' => b'-',
        b'+' => b'=',
        b'{' => b'[',
        b'}' => b']',
        b'|' => b'\\',
        b':' => b';',
        b'"' => b'\'',
        b'<' => b',',
        b'>' => b'.',
        b'?' => b'/',
        b'~' => b'`',
        _ => b,
    }
}

/// How many continuation bytes follow this byte in a UTF-8 sequence.
/// Returns `None` for non-lead bytes (ASCII or stray continuations).
pub(super) const fn utf8_continuation_count(b: u8) -> Option<u8> {
    match b {
        0xC2..=0xDF => Some(1),
        0xE0..=0xEF => Some(2),
        0xF0..=0xF4 => Some(3),
        _ => None,
    }
}

/// Kitty modifier code (`1 + bitfield`) to [`ModSet`]. Bits are shift, alt,
/// ctrl, super, hyper, meta, caps lock, num lock; `ModSet` has no hyper or
/// meta, so they fold into SUPER and ALT.
pub(super) fn kitty_modifier_code(code: u32) -> ModSet {
    const BITS: [ModSet; 8] = [
        ModSet::SHIFT,
        ModSet::ALT,
        ModSet::CTRL,
        ModSet::SUPER,
        ModSet::SUPER,
        ModSet::ALT,
        ModSet::CAPS_LOCK,
        ModSet::NUM_LOCK,
    ];
    let bits = code.saturating_sub(1);
    BITS.iter()
        .enumerate()
        .filter(|(bit, _)| bits & (1 << bit) != 0)
        .fold(ModSet::empty(), |mods, (_, m)| mods | *m)
}

/// Map a kitty keycode to a [`PhysicalKey`]: printable ASCII and a few
/// controls by value, functional keys by their kitty PUA codepoints, other
/// printable codepoints as `Unidentified` (the codepoint still travels in
/// `unshifted_codepoint`).
#[allow(
    clippy::too_many_lines,
    reason = "flat keycode table — the size IS the spec"
)]
pub(super) const fn kitty_keycode_to_physical(cp: u32) -> Option<PhysicalKey> {
    Some(match cp {
        27 => PhysicalKey::Escape,
        13 => PhysicalKey::Enter,
        9 => PhysicalKey::Tab,
        127 => PhysicalKey::Backspace,
        #[allow(clippy::cast_possible_truncation, reason = "range-checked u32 -> u8")]
        // Printable ASCII, punctuation included: under the host kitty flags
        // Ctrl+] arrives as `CSI 93;5u` and must stay `BracketRight`.
        c @ 0x20..=0x7E => ascii_to_physical(c as u8),
        // Functional keys (kitty PUA range).
        57348 => PhysicalKey::Insert,
        57349 => PhysicalKey::Delete,
        57350 => PhysicalKey::ArrowLeft,
        57351 => PhysicalKey::ArrowRight,
        57352 => PhysicalKey::ArrowUp,
        57353 => PhysicalKey::ArrowDown,
        57354 => PhysicalKey::PageUp,
        57355 => PhysicalKey::PageDown,
        57356 => PhysicalKey::Home,
        57357 => PhysicalKey::End,
        57358 => PhysicalKey::CapsLock,
        57359 => PhysicalKey::ScrollLock,
        57360 => PhysicalKey::NumLock,
        57361 => PhysicalKey::PrintScreen,
        57362 => PhysicalKey::Pause,
        57363 => PhysicalKey::ContextMenu,
        57364 => PhysicalKey::F1,
        57365 => PhysicalKey::F2,
        57366 => PhysicalKey::F3,
        57367 => PhysicalKey::F4,
        57368 => PhysicalKey::F5,
        57369 => PhysicalKey::F6,
        57370 => PhysicalKey::F7,
        57371 => PhysicalKey::F8,
        57372 => PhysicalKey::F9,
        57373 => PhysicalKey::F10,
        57374 => PhysicalKey::F11,
        57375 => PhysicalKey::F12,
        57376 => PhysicalKey::F13,
        57377 => PhysicalKey::F14,
        57378 => PhysicalKey::F15,
        57379 => PhysicalKey::F16,
        57380 => PhysicalKey::F17,
        57381 => PhysicalKey::F18,
        57382 => PhysicalKey::F19,
        57383 => PhysicalKey::F20,
        57384 => PhysicalKey::F21,
        57385 => PhysicalKey::F22,
        57386 => PhysicalKey::F23,
        57387 => PhysicalKey::F24,
        57388 => PhysicalKey::F25,
        // Numpad 0..9.
        57399 => PhysicalKey::Numpad0,
        57400 => PhysicalKey::Numpad1,
        57401 => PhysicalKey::Numpad2,
        57402 => PhysicalKey::Numpad3,
        57403 => PhysicalKey::Numpad4,
        57404 => PhysicalKey::Numpad5,
        57405 => PhysicalKey::Numpad6,
        57406 => PhysicalKey::Numpad7,
        57407 => PhysicalKey::Numpad8,
        57408 => PhysicalKey::Numpad9,
        57409 => PhysicalKey::NumpadDecimal,
        57410 => PhysicalKey::NumpadDivide,
        57411 => PhysicalKey::NumpadMultiply,
        57412 => PhysicalKey::NumpadSubtract,
        57413 => PhysicalKey::NumpadAdd,
        57414 => PhysicalKey::NumpadEnter,
        57415 => PhysicalKey::NumpadEqual,
        57416 => PhysicalKey::NumpadSeparator,
        57417 => PhysicalKey::NumpadLeft,
        57418 => PhysicalKey::NumpadRight,
        57419 => PhysicalKey::NumpadUp,
        57420 => PhysicalKey::NumpadDown,
        57421 => PhysicalKey::NumpadPageUp,
        57422 => PhysicalKey::NumpadPageDown,
        57423 => PhysicalKey::NumpadHome,
        57424 => PhysicalKey::NumpadEnd,
        57425 => PhysicalKey::NumpadInsert,
        57426 => PhysicalKey::NumpadDelete,
        57427 => PhysicalKey::NumpadBegin,
        // Modifier keys.
        57441 => PhysicalKey::ShiftLeft,
        57442 => PhysicalKey::ControlLeft,
        57443 => PhysicalKey::AltLeft,
        57444 => PhysicalKey::MetaLeft,
        57447 => PhysicalKey::ShiftRight,
        57448 => PhysicalKey::ControlRight,
        57449 => PhysicalKey::AltRight,
        57450 => PhysicalKey::MetaRight,
        _ if cp >= 0x20 => PhysicalKey::Unidentified,
        _ => return None,
    })
}

/// The [`PhysicalKey`] for an uppercase ASCII letter.
const fn letter_key(b: u8) -> PhysicalKey {
    match b {
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
        _ => PhysicalKey::Unidentified,
    }
}
