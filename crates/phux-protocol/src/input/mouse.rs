//! Mouse input: libghostty-free wire atoms whose discriminants match
//! libghostty's `mouse::{Action, Button}` (ADR-0024).

use super::key::ModSet;

/// What the mouse did. Wire `u32`; values match libghostty's `mouse::Action`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    /// Button pressed.
    Press = 0,
    /// Button released.
    Release = 1,
    /// Pointer moved.
    Motion = 2,
}

impl TryFrom<u32> for MouseAction {
    type Error = u32;
    fn try_from(v: u32) -> Result<Self, u32> {
        match v {
            0 => Ok(Self::Press),
            1 => Ok(Self::Release),
            2 => Ok(Self::Motion),
            other => Err(other),
        }
    }
}

/// Which mouse button. Wire `u32`; values match libghostty's `mouse::Button`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(missing_docs, reason = "button identities are self-explanatory")]
pub enum MouseButton {
    Unknown = 0,
    Left = 1,
    Right = 2,
    Middle = 3,
    Four = 4,
    Five = 5,
    Six = 6,
    Seven = 7,
    Eight = 8,
    Nine = 9,
    Ten = 10,
    Eleven = 11,
}

impl TryFrom<u32> for MouseButton {
    type Error = u32;
    fn try_from(v: u32) -> Result<Self, u32> {
        Ok(match v {
            0 => Self::Unknown,
            1 => Self::Left,
            2 => Self::Right,
            3 => Self::Middle,
            4 => Self::Four,
            5 => Self::Five,
            6 => Self::Six,
            7 => Self::Seven,
            8 => Self::Eight,
            9 => Self::Nine,
            10 => Self::Ten,
            11 => Self::Eleven,
            other => return Err(other),
        })
    }
}

/// A mouse input event. `x`/`y` are pane-local surface pixels, not cells
/// (docs/spec/input.md §3.1): cell-quantized clients MUST send
/// `cell_index × cell_size`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MouseEvent {
    /// What the mouse did: press, release, or motion.
    pub action: MouseAction,
    /// Which button was involved.
    pub button: MouseButton,
    /// Keyboard modifiers held during the event.
    pub mods: ModSet,
    /// Pane-local surface-space pixel X.
    pub x: f64,
    /// Pane-local surface-space pixel Y.
    pub y: f64,
}

/// Server-side mouse state handles + atom conversions (libghostty boundary).
#[cfg(feature = "server")]
mod server_side {
    use super::{MouseAction, MouseButton};

    /// The inner program's DECSET mouse-tracking state (not a wire field).
    pub use libghostty_vt::mouse::{Format as MouseEncoding, TrackingMode as MouseProtocol};

    impl From<MouseAction> for libghostty_vt::mouse::Action {
        fn from(a: MouseAction) -> Self {
            match a {
                MouseAction::Press => Self::Press,
                MouseAction::Release => Self::Release,
                MouseAction::Motion => Self::Motion,
            }
        }
    }

    impl From<MouseButton> for libghostty_vt::mouse::Button {
        fn from(b: MouseButton) -> Self {
            match b {
                MouseButton::Unknown => Self::Unknown,
                MouseButton::Left => Self::Left,
                MouseButton::Right => Self::Right,
                MouseButton::Middle => Self::Middle,
                MouseButton::Four => Self::Four,
                MouseButton::Five => Self::Five,
                MouseButton::Six => Self::Six,
                MouseButton::Seven => Self::Seven,
                MouseButton::Eight => Self::Eight,
                MouseButton::Nine => Self::Nine,
                MouseButton::Ten => Self::Ten,
                MouseButton::Eleven => Self::Eleven,
            }
        }
    }
}

#[cfg(feature = "server")]
pub use server_side::{MouseEncoding, MouseProtocol};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_atoms_round_trip_wire_discriminants() {
        for a in [
            MouseAction::Press,
            MouseAction::Release,
            MouseAction::Motion,
        ] {
            assert_eq!(MouseAction::try_from(a as u32), Ok(a));
        }
        for b in [MouseButton::Left, MouseButton::Middle, MouseButton::Eleven] {
            assert_eq!(MouseButton::try_from(b as u32), Ok(b));
        }
        assert!(MouseAction::try_from(99).is_err());
        assert!(MouseButton::try_from(99).is_err());
    }
}
