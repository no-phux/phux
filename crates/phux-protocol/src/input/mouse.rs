//! Mouse input: libghostty-free wire atoms whose discriminants match
//! libghostty's `mouse::{Action, Button}` (ADR-0024).

use super::key::ModSet;

/// What the mouse did. Wire `u32`; values match libghostty's `mouse::Action`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, int_enum::IntEnum)]
pub enum MouseAction {
    /// Button pressed.
    Press = 0,
    /// Button released.
    Release = 1,
    /// Pointer moved.
    Motion = 2,
}

/// Which mouse button. Wire `u32`; values match libghostty's `mouse::Button`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, int_enum::IntEnum)]
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

/// A mouse input event. `x`/`y` are pane-local surface pixels, not cells
/// (docs/protocol.md#input): cell-quantized clients MUST send
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
