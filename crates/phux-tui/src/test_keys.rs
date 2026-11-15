//! Key-event constructors shared by the crate's tests.
#![allow(clippy::redundant_pub_crate, reason = "crate-wide test helpers")]

use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

/// A press of `key` holding `mods`, producing `text`.
pub(crate) fn key_event(key: PhysicalKey, mods: ModSet, text: Option<&str>) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods,
        consumed_mods: ModSet::empty(),
        composing: false,
        text: text.map(ToOwned::to_owned),
        unshifted_codepoint: None,
    }
}

/// A bare press of `key`: no modifiers, no text.
pub(crate) fn press(key: PhysicalKey) -> KeyEvent {
    key_event(key, ModSet::empty(), None)
}

/// An unmodified press of `key` producing `text`.
pub(crate) fn press_text(key: PhysicalKey, text: Option<&str>) -> KeyEvent {
    key_event(key, ModSet::empty(), text)
}

/// A press of `key` holding `mods`, with no text.
pub(crate) fn press_mods(key: PhysicalKey, mods: ModSet) -> KeyEvent {
    key_event(key, mods, None)
}
