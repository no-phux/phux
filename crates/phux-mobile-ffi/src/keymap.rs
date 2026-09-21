//! Key input mapping — Swift keypresses to wire `KeyEvent`s.
//!
//! The native edition of `clients/phux-web/src/client.rs`'s browser mapping
//! (`key_event_from_browser` + `code_to_physical_key`): Swift reports what the
//! user pressed — a layout-resolved character or a named key, plus held
//! modifiers — and this module builds the layout-independent wire `KeyEvent`.
//! The mapping lives here, not in Swift, so the `PhysicalKey`/`ModSet`
//! discriminants stay pinned to the `phux-protocol` source of truth and are
//! unit-testable with plain `cargo test` (no Xcode, no zig).
//!
//! Mirrored phux-web rules:
//! - every press is `KeyAction::Press` (the browser client sends no
//!   Release/Repeat either);
//! - `text` is carried only for printable presses without Ctrl/Meta — the
//!   server's encoder derives control bytes from `key + mods`;
//! - `text` never contains C0 control characters (the wire forbids them);
//!   control characters in typed text are routed to named keys or Ctrl+letter.

use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

/// What the user pressed, as Swift reports it.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum KeyPress {
    /// A printable keypress carrying its layout-resolved character —
    /// letters, digits, symbols, space.
    Character { text: String },
    /// A non-printable named key (accessory bar, hardware keyboard).
    Named { key: NamedKey },
}

/// The named non-printable keys a terminal needs — the same set phux-web maps
/// from `KeyboardEvent.code` by name.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedKey {
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Insert,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// Modifier state at the moment of the keypress.
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent physical modifiers are the wire and platform input model"
)]
#[derive(uniffi::Record, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyMods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Super / Command.
    pub meta: bool,
}

impl KeyMods {
    /// Ctrl only — the common accessory-bar chord (Ctrl-C etc.).
    #[cfg(test)]
    pub(crate) const fn ctrl() -> Self {
        Self {
            ctrl: true,
            alt: false,
            shift: false,
            meta: false,
        }
    }

    pub(crate) fn to_mod_set(self) -> ModSet {
        let mut mods = ModSet::empty();
        if self.ctrl {
            mods |= ModSet::CTRL;
        }
        if self.alt {
            mods |= ModSet::ALT;
        }
        if self.shift {
            mods |= ModSet::SHIFT;
        }
        if self.meta {
            mods |= ModSet::SUPER;
        }
        mods
    }
}

/// Build the wire `KeyEvent` for one keypress. Always a `Press` (phux-web
/// parity: no Release/Repeat events are sent).
pub(crate) fn key_event(press: &KeyPress, mods: KeyMods) -> KeyEvent {
    let (key, text) = match press {
        KeyPress::Character { text } => {
            let key = text
                .chars()
                .next()
                .map_or(PhysicalKey::Unidentified, physical_key_for_char);
            // phux-web: carry text only without Ctrl/Meta; the server's
            // encoder derives control bytes from key + mods.
            let carry = !mods.ctrl && !mods.meta && !text.is_empty();
            (key, carry.then(|| text.clone()))
        }
        KeyPress::Named { key } => (physical_key_for_named(*key), None),
    };
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods: mods.to_mod_set(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text,
        unshifted_codepoint: None,
    }
}

/// Split typed text (the line-compose commit path) into per-key presses.
/// Control characters become named keys or Ctrl+letter chords — the wire
/// forbids C0 bytes in `text`. Unmappable C0/PUA codes are dropped.
#[cfg(test)]
pub(crate) fn presses_for_text(text: &str) -> Vec<(KeyPress, KeyMods)> {
    text.chars()
        .filter_map(|ch| match ch {
            '\r' | '\n' => Some((named(NamedKey::Enter), KeyMods::default())),
            '\t' => Some((named(NamedKey::Tab), KeyMods::default())),
            '\u{8}' | '\u{7f}' => Some((named(NamedKey::Backspace), KeyMods::default())),
            '\u{1b}' => Some((named(NamedKey::Escape), KeyMods::default())),
            // Remaining C0 controls 0x01..=0x1A are Ctrl+letter (0x03 = Ctrl-C).
            '\u{1}'..='\u{1a}' => {
                let letter = char::from(b'a' + (ch as u8 - 1));
                Some((
                    KeyPress::Character {
                        text: letter.to_string(),
                    },
                    KeyMods {
                        ctrl: true,
                        ..KeyMods::default()
                    },
                ))
            }
            // Other C0 (0x00, 0x1C..=0x1F) and PUA function codes: drop.
            c if (c as u32) < 0x20 || (0xF700..=0xF8FF).contains(&(c as u32)) => None,
            c => Some((
                KeyPress::Character {
                    text: c.to_string(),
                },
                KeyMods::default(),
            )),
        })
        .collect()
}

#[cfg(test)]
fn named(key: NamedKey) -> KeyPress {
    KeyPress::Named { key }
}

/// Physical key for a layout-resolved character, US-layout best effort
/// (phux-web parity: letters/digits arithmetic, punctuation by name). Shifted
/// symbols map to their base key; the produced character travels in `text`,
/// so the server never has to re-derive it from key + mods. Unknown characters
/// fall back to `Unidentified` — the server still gets them via `text`.
fn physical_key_for_char(ch: char) -> PhysicalKey {
    use PhysicalKey as K;
    match ch {
        'a'..='z' => letter_key(ch as u32 - 'a' as u32),
        'A'..='Z' => letter_key(ch as u32 - 'A' as u32),
        '0'..='9' => digit_key(ch as u32 - '0' as u32),
        ' ' => K::Space,
        '-' | '_' => K::Minus,
        '=' | '+' => K::Equal,
        '[' | '{' => K::BracketLeft,
        ']' | '}' => K::BracketRight,
        '\\' | '|' => K::Backslash,
        ';' | ':' => K::Semicolon,
        '\'' | '"' => K::Quote,
        ',' | '<' => K::Comma,
        '.' | '>' => K::Period,
        '/' | '?' => K::Slash,
        '`' | '~' => K::Backquote,
        '!' => K::Digit1,
        '@' => K::Digit2,
        '#' => K::Digit3,
        '$' => K::Digit4,
        '%' => K::Digit5,
        '^' => K::Digit6,
        '&' => K::Digit7,
        '*' => K::Digit8,
        '(' => K::Digit9,
        ')' => K::Digit0,
        _ => K::Unidentified,
    }
}

/// `A = 20` .. `Z = 45` (phux-protocol discriminants, ADR-0024).
fn letter_key(offset: u32) -> PhysicalKey {
    PhysicalKey::try_from(20 + offset).unwrap_or(PhysicalKey::Unidentified)
}

/// `Digit0 = 6` .. `Digit9 = 15`.
fn digit_key(offset: u32) -> PhysicalKey {
    PhysicalKey::try_from(6 + offset).unwrap_or(PhysicalKey::Unidentified)
}

fn physical_key_for_named(key: NamedKey) -> PhysicalKey {
    use PhysicalKey as K;
    match key {
        NamedKey::Enter => K::Enter,
        NamedKey::Tab => K::Tab,
        NamedKey::Escape => K::Escape,
        NamedKey::Backspace => K::Backspace,
        NamedKey::Delete => K::Delete,
        NamedKey::Insert => K::Insert,
        NamedKey::ArrowUp => K::ArrowUp,
        NamedKey::ArrowDown => K::ArrowDown,
        NamedKey::ArrowLeft => K::ArrowLeft,
        NamedKey::ArrowRight => K::ArrowRight,
        NamedKey::Home => K::Home,
        NamedKey::End => K::End,
        NamedKey::PageUp => K::PageUp,
        NamedKey::PageDown => K::PageDown,
        NamedKey::F1 => K::F1,
        NamedKey::F2 => K::F2,
        NamedKey::F3 => K::F3,
        NamedKey::F4 => K::F4,
        NamedKey::F5 => K::F5,
        NamedKey::F6 => K::F6,
        NamedKey::F7 => K::F7,
        NamedKey::F8 => K::F8,
        NamedKey::F9 => K::F9,
        NamedKey::F10 => K::F10,
        NamedKey::F11 => K::F11,
        NamedKey::F12 => K::F12,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(press: KeyPress, mods: KeyMods) -> KeyEvent {
        key_event(&press, mods)
    }

    fn ch(s: &str) -> KeyPress {
        KeyPress::Character { text: s.to_owned() }
    }

    #[test]
    fn letters_map_arithmetically_and_carry_text() {
        let e = event(ch("a"), KeyMods::default());
        assert_eq!(e.key, PhysicalKey::A);
        assert_eq!(e.text.as_deref(), Some("a"));
        assert_eq!(e.action, KeyAction::Press);
        assert!(e.mods.is_empty());

        let e = event(ch("z"), KeyMods::default());
        assert_eq!(e.key, PhysicalKey::Z);

        // Uppercase resolves to the same physical key; the case travels in text.
        let e = event(ch("Q"), KeyMods::default());
        assert_eq!(e.key, PhysicalKey::Q);
        assert_eq!(e.text.as_deref(), Some("Q"));
    }

    #[test]
    fn digits_and_symbols_map_to_us_layout_keys() {
        assert_eq!(event(ch("0"), KeyMods::default()).key, PhysicalKey::Digit0);
        assert_eq!(event(ch("9"), KeyMods::default()).key, PhysicalKey::Digit9);
        assert_eq!(event(ch("!"), KeyMods::default()).key, PhysicalKey::Digit1);
        assert_eq!(event(ch(")"), KeyMods::default()).key, PhysicalKey::Digit0);
        assert_eq!(event(ch("-"), KeyMods::default()).key, PhysicalKey::Minus);
        assert_eq!(event(ch("_"), KeyMods::default()).key, PhysicalKey::Minus);
        assert_eq!(event(ch("/"), KeyMods::default()).key, PhysicalKey::Slash);
        assert_eq!(
            event(ch("~"), KeyMods::default()).key,
            PhysicalKey::Backquote
        );
        assert_eq!(event(ch(" "), KeyMods::default()).key, PhysicalKey::Space);
        // Unknown characters still carry their text.
        let e = event(ch("é"), KeyMods::default());
        assert_eq!(e.key, PhysicalKey::Unidentified);
        assert_eq!(e.text.as_deref(), Some("é"));
    }

    #[test]
    fn ctrl_chords_drop_text_and_set_mods() {
        // Ctrl-C: the encoder derives 0x03 from key + mods; text must be None.
        let e = event(ch("c"), KeyMods::ctrl());
        assert_eq!(e.key, PhysicalKey::C);
        assert_eq!(e.text, None);
        assert!(e.mods.contains(ModSet::CTRL));

        // Meta also suppresses text (phux-web parity); plain Shift/Alt do not.
        let meta = KeyMods {
            meta: true,
            ..KeyMods::default()
        };
        assert_eq!(event(ch("v"), meta).text, None);
        let shift = KeyMods {
            shift: true,
            ..KeyMods::default()
        };
        assert_eq!(event(ch("V"), shift).text.as_deref(), Some("V"));
    }

    #[test]
    fn named_keys_map_to_protocol_discriminants() {
        let cases: [(NamedKey, u32); 10] = [
            (NamedKey::Enter, 58),
            (NamedKey::Tab, 64),
            (NamedKey::Escape, 120),
            (NamedKey::Backspace, 53),
            (NamedKey::Delete, 68),
            (NamedKey::ArrowUp, 78),
            (NamedKey::ArrowDown, 75),
            (NamedKey::ArrowLeft, 76),
            (NamedKey::ArrowRight, 77),
            (NamedKey::F1, 121),
        ];
        for (key, discriminant) in cases {
            let e = event(named(key), KeyMods::default());
            assert_eq!(e.key as u32, discriminant, "{key:?}");
            assert_eq!(e.text, None, "{key:?} must not carry text");
        }
    }

    #[test]
    fn modified_named_keys_keep_mods() {
        let e = event(
            named(NamedKey::ArrowLeft),
            KeyMods {
                alt: true,
                ..KeyMods::default()
            },
        );
        assert!(e.mods.contains(ModSet::ALT));
        assert_eq!(e.key, PhysicalKey::ArrowLeft);
    }

    #[test]
    fn typed_text_routes_control_chars_to_named_keys() {
        let presses = presses_for_text("ls\r");
        assert_eq!(presses.len(), 3);
        assert_eq!(presses[0].0, ch("l"));
        assert_eq!(presses[1].0, ch("s"));
        assert_eq!(presses[2].0, named(NamedKey::Enter));

        // No press in the commit path may carry a C0 byte in text.
        for (press, mods) in presses_for_text("a\tb\u{8}c\u{1b}\n") {
            let e = key_event(&press, mods);
            if let Some(text) = &e.text {
                assert!(
                    text.chars().all(|c| c as u32 >= 0x20 && c as u32 != 0x7f),
                    "C0 leaked into text: {text:?}"
                );
            }
        }
    }

    #[test]
    fn typed_control_bytes_become_ctrl_chords() {
        // A raw 0x03 (Ctrl-C) typed/pasted into the stream.
        let presses = presses_for_text("\u{3}");
        assert_eq!(presses.len(), 1);
        let e = key_event(&presses[0].0, presses[0].1);
        assert_eq!(e.key, PhysicalKey::C);
        assert!(e.mods.contains(ModSet::CTRL));
        assert_eq!(e.text, None);
    }

    #[test]
    fn unmappable_controls_are_dropped() {
        assert!(presses_for_text("\u{0}\u{1c}\u{f700}").is_empty());
    }
}
