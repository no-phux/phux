//! Typed text as key events: the raw-passthrough path every consumer uses
//! to type a string (the line-compose commit, an accessory bar, a test).
//!
//! Control characters become named keys or Ctrl+letter chords, because the
//! wire forbids C0 bytes in a key's `text` (`docs/spec/input.md` section 2).
//! Unmappable C0 and platform PUA function codes are dropped.

use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

/// The key events that type `text`, one per scalar.
#[must_use]
pub fn key_events_for_text(text: &str) -> Vec<KeyEvent> {
    text.chars().filter_map(key_event_for_char).collect()
}

/// The key event that types one scalar, or `None` for an unmappable one.
#[must_use]
pub fn key_event_for_char(ch: char) -> Option<KeyEvent> {
    match ch {
        '\r' | '\n' => Some(named(PhysicalKey::Enter)),
        '\t' => Some(named(PhysicalKey::Tab)),
        '\u{8}' | '\u{7f}' => Some(named(PhysicalKey::Backspace)),
        '\u{1b}' => Some(named(PhysicalKey::Escape)),
        // Remaining C0 controls 0x01..=0x1A are Ctrl+letter (0x03 = Ctrl-C).
        '\u{1}'..='\u{1a}' => {
            let letter = char::from(b'a' + (ch as u8 - 1));
            Some(press(
                physical_key_for_char(letter),
                ModSet::CTRL,
                // The server's encoder derives the control byte from key
                // plus mods; carrying the letter would type it.
                None,
            ))
        }
        // Other C0 (0x00, 0x1C..=0x1F) and PUA function codes: drop.
        c if (c as u32) < 0x20 || (0xF700..=0xF8FF).contains(&(c as u32)) => None,
        c => Some(press(
            physical_key_for_char(c),
            ModSet::empty(),
            Some(c.to_string()),
        )),
    }
}

/// A key press with no text, as a named key is sent.
#[must_use]
pub const fn named(key: PhysicalKey) -> KeyEvent {
    press(key, ModSet::empty(), None)
}

/// A key press.
#[must_use]
pub const fn press(key: PhysicalKey, mods: ModSet, text: Option<String>) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods,
        consumed_mods: ModSet::empty(),
        composing: false,
        text,
        unshifted_codepoint: None,
    }
}

/// Physical key for a layout-resolved character, US-layout best effort.
///
/// Letters and digits by arithmetic, punctuation by name. Shifted symbols
/// map to their base key; the produced character travels in `text`, so the
/// server never re-derives it from key plus mods. Unknown characters fall
/// back to `Unidentified`; the server still receives them via `text`.
#[must_use]
pub fn physical_key_for_char(ch: char) -> PhysicalKey {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_becomes_one_press_per_scalar_with_controls_named() {
        let events = key_events_for_text("a\n\x03\t");
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].key, PhysicalKey::A);
        assert_eq!(events[0].text.as_deref(), Some("a"));
        assert_eq!(events[1].key, PhysicalKey::Enter);
        assert_eq!(events[1].text, None);
        assert_eq!(events[2].key, PhysicalKey::C);
        assert_eq!(events[2].mods, ModSet::CTRL);
        assert_eq!(events[2].text, None, "a control chord carries no text");
        assert_eq!(events[3].key, PhysicalKey::Tab);
    }

    #[test]
    fn unmappable_controls_are_dropped_and_symbols_keep_their_text() {
        assert!(key_events_for_text("\u{0}\u{1c}\u{f700}").is_empty());
        let bang = key_event_for_char('!').expect("mapped");
        assert_eq!(bang.key, PhysicalKey::Digit1);
        assert_eq!(bang.text.as_deref(), Some("!"));
        assert_eq!(physical_key_for_char('9'), PhysicalKey::Digit9);
        assert_eq!(physical_key_for_char('\u{6f22}'), PhysicalKey::Unidentified);
    }
}
