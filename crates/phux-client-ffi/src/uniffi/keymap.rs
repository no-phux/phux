//! Key input mapping: Swift keypresses to wire `KeyEvent`s, with phux-web's
//! rules. Every event is a `Press`; `text` rides only on printable presses
//! without Ctrl/Meta (the server derives control bytes from `key + mods`).
//! Character-to-key mapping is the runtime's
//! [`physical_key_for_char`](phux_client_runtime::control::keys::physical_key_for_char).

use phux_client_runtime::control::keys::physical_key_for_char;
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
}
