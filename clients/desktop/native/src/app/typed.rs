//! Ghostty `text:` / `esc:` as structured keys.
//!
//! LF is Ctrl-J, not Enter. ESC before a key is Alt on that key. This matches the
//! desktop host's `typeText` path; `key_events_for_text` is the wrong mapping.

use phux_client_core::keys::{key_event_for_char, press};
use phux_protocol::input::key::{KeyEvent, ModSet, PhysicalKey};

pub fn typed_keys(text: &str) -> Vec<KeyEvent> {
    let mut events = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        let meta = (ch == '\u{1b}')
            .then(|| chars.peek().copied())
            .flatten()
            .filter(|next| *next != '\u{1b}')
            .and_then(typed_key);
        if let Some(mut event) = meta {
            chars.next();
            event.mods.insert(ModSet::ALT);
            events.push(event);
        } else if let Some(event) = typed_key(ch) {
            events.push(event);
        }
    }
    events
}

fn typed_key(ch: char) -> Option<KeyEvent> {
    let control = |key| Some(press(key, ModSet::CTRL, None));
    match ch {
        '\n' => control(PhysicalKey::J),
        '\u{8}' => control(PhysicalKey::H),
        '\u{0}' => control(PhysicalKey::Space),
        '\u{1c}' => control(PhysicalKey::Backslash),
        '\u{1d}' => control(PhysicalKey::BracketRight),
        '\u{1e}' => control(PhysicalKey::Digit6),
        '\u{1f}' => control(PhysicalKey::Minus),
        other => key_event_for_char(other),
    }
}
