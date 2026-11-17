//! Conversions between phux's wire input atoms and libghostty's, at the
//! engine boundary.
//!
//! The wire atoms are libghostty-free (so the published codec builds for
//! wasm) but share libghostty's discriminants (ADR-0024). Neither side is
//! local to this crate, so the orphan rule rules out `From` impls; these
//! extension traits stand in for them.

use libghostty_vt::{focus, key, mouse};
use phux_protocol::input::{
    focus::FocusEvent,
    key::{KeyAction, ModSet, PhysicalKey},
    mouse::{MouseAction, MouseButton},
};

/// The inner program's DECSET mouse-tracking state (not a wire field).
pub use libghostty_vt::mouse::{Format as MouseEncoding, TrackingMode as MouseProtocol};

/// A wire atom with a libghostty counterpart.
pub trait ToGhostty {
    /// The libghostty atom.
    type Ghostty;

    /// Convert to the libghostty atom.
    fn to_ghostty(self) -> Self::Ghostty;
}

/// A wire atom recoverable from a libghostty atom.
pub trait FromGhostty<T> {
    /// Convert from the libghostty atom.
    fn from_ghostty(value: T) -> Self;
}

impl ToGhostty for KeyAction {
    type Ghostty = key::Action;

    fn to_ghostty(self) -> key::Action {
        match self {
            Self::Release => key::Action::Release,
            Self::Press => key::Action::Press,
            Self::Repeat => key::Action::Repeat,
        }
    }
}

impl FromGhostty<key::Action> for KeyAction {
    fn from_ghostty(value: key::Action) -> Self {
        match value {
            key::Action::Release => Self::Release,
            key::Action::Repeat => Self::Repeat,
            // `Action` is #[non_exhaustive]; Press is the safe default.
            _ => Self::Press,
        }
    }
}

impl ToGhostty for ModSet {
    type Ghostty = key::Mods;

    fn to_ghostty(self) -> key::Mods {
        key::Mods::from_bits_truncate(self.bits())
    }
}

impl FromGhostty<key::Mods> for ModSet {
    fn from_ghostty(value: key::Mods) -> Self {
        Self::from_bits_truncate(value.bits())
    }
}

impl ToGhostty for PhysicalKey {
    type Ghostty = key::Key;

    fn to_ghostty(self) -> key::Key {
        // Same discriminants (ADR-0024); unknown -> Unidentified.
        key::Key::try_from(self as i32).unwrap_or(key::Key::Unidentified)
    }
}

impl FromGhostty<key::Key> for PhysicalKey {
    fn from_ghostty(value: key::Key) -> Self {
        Self::try_from(value as u32).unwrap_or(Self::Unidentified)
    }
}

impl ToGhostty for MouseAction {
    type Ghostty = mouse::Action;

    fn to_ghostty(self) -> mouse::Action {
        match self {
            Self::Press => mouse::Action::Press,
            Self::Release => mouse::Action::Release,
            Self::Motion => mouse::Action::Motion,
        }
    }
}

impl ToGhostty for MouseButton {
    type Ghostty = mouse::Button;

    fn to_ghostty(self) -> mouse::Button {
        match self {
            Self::Unknown => mouse::Button::Unknown,
            Self::Left => mouse::Button::Left,
            Self::Right => mouse::Button::Right,
            Self::Middle => mouse::Button::Middle,
            Self::Four => mouse::Button::Four,
            Self::Five => mouse::Button::Five,
            Self::Six => mouse::Button::Six,
            Self::Seven => mouse::Button::Seven,
            Self::Eight => mouse::Button::Eight,
            Self::Nine => mouse::Button::Nine,
            Self::Ten => mouse::Button::Ten,
            Self::Eleven => mouse::Button::Eleven,
        }
    }
}

impl ToGhostty for FocusEvent {
    type Ghostty = focus::Event;

    fn to_ghostty(self) -> focus::Event {
        match self {
            Self::Gained => focus::Event::Gained,
            Self::Lost => focus::Event::Lost,
        }
    }
}

impl FromGhostty<focus::Event> for FocusEvent {
    fn from_ghostty(value: focus::Event) -> Self {
        match value {
            focus::Event::Gained => Self::Gained,
            focus::Event::Lost => Self::Lost,
        }
    }
}
