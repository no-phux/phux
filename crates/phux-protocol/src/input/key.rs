//! Key input — the `KeyEvent` wire type and its atoms.
//!
//! Per [ADR-0024] the wire owns its input atoms: `KeyAction`, `ModSet`, and
//! `PhysicalKey` are phux-defined and libghostty-free, so the codec builds for
//! non-native consumers (the wasm browser client). Their wire discriminants
//! match libghostty-vt's `key::{Action, Mods, Key}` exactly; under the `server`
//! feature this module provides the `From` conversions the server's encoders
//! use at the libghostty boundary.
//!
//! [ADR-0024]: https://github.com/phall1/phux/blob/main/ADR/0024-wire-owns-input-atoms.md

/// Press, release, or repeat. Wire `u32`; values match libghostty's
/// `key::Action`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Key was released.
    Release = 0,
    /// Key was pressed.
    Press = 1,
    /// Key is being held down (auto-repeat).
    Repeat = 2,
}

impl KeyAction {
    /// The wire discriminant.
    #[must_use]
    pub const fn to_u32(self) -> u32 {
        self as u32
    }

    /// Build from a wire discriminant; `None` if unknown.
    #[must_use]
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Release),
            1 => Some(Self::Press),
            2 => Some(Self::Repeat),
            _ => None,
        }
    }
}

impl TryFrom<u32> for KeyAction {
    type Error = u32;
    fn try_from(v: u32) -> Result<Self, u32> {
        Self::from_u32(v).ok_or(v)
    }
}

bitflags::bitflags! {
    /// Keyboard modifier bitset. Wire `u16`; bits match libghostty's `key::Mods`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ModSet: u16 {
        /// Shift.
        const SHIFT = 1;
        /// Control.
        const CTRL = 2;
        /// Alt / Option.
        const ALT = 4;
        /// Super / Command / Windows.
        const SUPER = 8;
        /// Caps Lock active.
        const CAPS_LOCK = 16;
        /// Num Lock active.
        const NUM_LOCK = 32;
        /// Shift held on the right side.
        const SHIFT_SIDE = 64;
        /// Control held on the right side.
        const CTRL_SIDE = 128;
        /// Alt held on the right side.
        const ALT_SIDE = 256;
        /// Super held on the right side.
        const SUPER_SIDE = 512;
    }
}

/// A physical, layout-independent key (W3C `code`-style).
///
/// Per [ADR-0024] this is a phux-owned copy of libghostty's `key::Key`
/// discriminants (wire `u32`), so the codec builds for non-native consumers.
/// Kept in lockstep with libghostty via the `server`-gated conversions + a
/// round-trip test. Browser consumers map `KeyboardEvent.code` to these.
///
/// [ADR-0024]: https://github.com/phall1/phux/blob/main/ADR/0024-wire-owns-input-atoms.md
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, int_enum::IntEnum)]
#[non_exhaustive]
#[expect(missing_docs, reason = "W3C key codes are self-explanatory")]
pub enum PhysicalKey {
    Unidentified = 0,
    Backquote = 1,
    Backslash = 2,
    BracketLeft = 3,
    BracketRight = 4,
    Comma = 5,
    Digit0 = 6,
    Digit1 = 7,
    Digit2 = 8,
    Digit3 = 9,
    Digit4 = 10,
    Digit5 = 11,
    Digit6 = 12,
    Digit7 = 13,
    Digit8 = 14,
    Digit9 = 15,
    Equal = 16,
    IntlBackslash = 17,
    IntlRo = 18,
    IntlYen = 19,
    A = 20,
    B = 21,
    C = 22,
    D = 23,
    E = 24,
    F = 25,
    G = 26,
    H = 27,
    I = 28,
    J = 29,
    K = 30,
    L = 31,
    M = 32,
    N = 33,
    O = 34,
    P = 35,
    Q = 36,
    R = 37,
    S = 38,
    T = 39,
    U = 40,
    V = 41,
    W = 42,
    X = 43,
    Y = 44,
    Z = 45,
    Minus = 46,
    Period = 47,
    Quote = 48,
    Semicolon = 49,
    Slash = 50,
    AltLeft = 51,
    AltRight = 52,
    Backspace = 53,
    CapsLock = 54,
    ContextMenu = 55,
    ControlLeft = 56,
    ControlRight = 57,
    Enter = 58,
    MetaLeft = 59,
    MetaRight = 60,
    ShiftLeft = 61,
    ShiftRight = 62,
    Space = 63,
    Tab = 64,
    Convert = 65,
    KanaMode = 66,
    NonConvert = 67,
    Delete = 68,
    End = 69,
    Help = 70,
    Home = 71,
    Insert = 72,
    PageDown = 73,
    PageUp = 74,
    ArrowDown = 75,
    ArrowLeft = 76,
    ArrowRight = 77,
    ArrowUp = 78,
    NumLock = 79,
    Numpad0 = 80,
    Numpad1 = 81,
    Numpad2 = 82,
    Numpad3 = 83,
    Numpad4 = 84,
    Numpad5 = 85,
    Numpad6 = 86,
    Numpad7 = 87,
    Numpad8 = 88,
    Numpad9 = 89,
    NumpadAdd = 90,
    NumpadBackspace = 91,
    NumpadClear = 92,
    NumpadClearEntry = 93,
    NumpadComma = 94,
    NumpadDecimal = 95,
    NumpadDivide = 96,
    NumpadEnter = 97,
    NumpadEqual = 98,
    NumpadMemoryAdd = 99,
    NumpadMemoryClear = 100,
    NumpadMemoryRecall = 101,
    NumpadMemoryStore = 102,
    NumpadMemorySubtract = 103,
    NumpadMultiply = 104,
    NumpadParenLeft = 105,
    NumpadParenRight = 106,
    NumpadSubtract = 107,
    NumpadSeparator = 108,
    NumpadUp = 109,
    NumpadDown = 110,
    NumpadRight = 111,
    NumpadLeft = 112,
    NumpadBegin = 113,
    NumpadHome = 114,
    NumpadEnd = 115,
    NumpadInsert = 116,
    NumpadDelete = 117,
    NumpadPageUp = 118,
    NumpadPageDown = 119,
    Escape = 120,
    F1 = 121,
    F2 = 122,
    F3 = 123,
    F4 = 124,
    F5 = 125,
    F6 = 126,
    F7 = 127,
    F8 = 128,
    F9 = 129,
    F10 = 130,
    F11 = 131,
    F12 = 132,
    F13 = 133,
    F14 = 134,
    F15 = 135,
    F16 = 136,
    F17 = 137,
    F18 = 138,
    F19 = 139,
    F20 = 140,
    F21 = 141,
    F22 = 142,
    F23 = 143,
    F24 = 144,
    F25 = 145,
    Fn = 146,
    FnLock = 147,
    PrintScreen = 148,
    ScrollLock = 149,
    Pause = 150,
    BrowserBack = 151,
    BrowserFavorites = 152,
    BrowserForward = 153,
    BrowserHome = 154,
    BrowserRefresh = 155,
    BrowserSearch = 156,
    BrowserStop = 157,
    Eject = 158,
    LaunchApp1 = 159,
    LaunchApp2 = 160,
    LaunchMail = 161,
    MediaPlayPause = 162,
    MediaSelect = 163,
    MediaStop = 164,
    MediaTrackNext = 165,
    MediaTrackPrevious = 166,
    Power = 167,
    Sleep = 168,
    AudioVolumeDown = 169,
    AudioVolumeMute = 170,
    AudioVolumeUp = 171,
    WakeUp = 172,
    Copy = 173,
    Cut = 174,
    Paste = 175,
}

/// One key event on a pane.
///
/// Layout-independent: `key` is the physical (W3C `code`-style) key; `text` and
/// `unshifted_codepoint` carry the layout-resolved character.
///
/// See docs/spec/input.md §2 for field semantics.
///
/// `Debug` is hand-written and **redaction-safe** (ADR-0028): it never prints
/// the layout-resolved `text` or `unshifted_codepoint`, since together those
/// reconstruct the user's literal keystrokes (passwords, secrets). It reports
/// only the structural facts an operator needs in a log — action, physical
/// key, modifiers, and whether text was present (as a length, never the bytes)
/// — so the common `trace!(?event)` / `trace!(?input)` call sites are safe by
/// construction without each site having to remember to redact.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyEvent {
    /// Press, release, or repeat.
    pub action: KeyAction,
    /// The physical key, independent of layout or modifiers.
    pub key: PhysicalKey,
    /// Modifier bitset at the moment of the event.
    pub mods: ModSet,
    /// Subset of `mods` consumed by the OS to produce `text`. KIP's encoder
    /// uses this to avoid double-applying modifiers in escape sequences.
    /// Clients without this information SHOULD pass [`ModSet::empty`].
    pub consumed_mods: ModSet,
    /// True if this event is part of an active IME composition sequence.
    pub composing: bool,
    /// UTF-8 text produced by this keypress under the current layout, before
    /// any Ctrl/Meta transformation. MUST NOT contain C0 control characters
    /// (`U+0000..=U+001F`, `U+007F`) nor platform PUA function-key codes
    /// (`U+F700..=U+F8FF`) — pass `None` and let the encoder derive the
    /// bytes from `key + mods`.
    pub text: Option<String>,
    /// Layout-resolved codepoint that would have been produced with no
    /// modifiers held. Used by KIP `REPORT_ALTERNATES`.
    pub unshifted_codepoint: Option<u32>,
}

/// Redaction-safe `Debug` (ADR-0028).
///
/// Emits the event's structure but never its content: `text` is reduced to
/// `text_len` (a UTF-8 byte count) and `unshifted_codepoint` to a present/absent
/// boolean. This is what lands in logs whenever a `KeyEvent` (or an
/// `InputEvent` wrapping one) is formatted with `{:?}` — e.g. the server's
/// `trace!(?input, …)` PTY-handoff diagnostics. The physical key and modifiers
/// are structural (already plain integers on the wire) and safe to log.
impl core::fmt::Debug for KeyEvent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("KeyEvent")
            .field("action", &self.action)
            .field("key", &self.key)
            .field("mods", &self.mods)
            .field("consumed_mods", &self.consumed_mods)
            .field("composing", &self.composing)
            // Redacted: never log the literal characters produced.
            .field("text_len", &self.text.as_ref().map(String::len))
            .field("has_unshifted_codepoint", &self.unshifted_codepoint.is_some())
            .finish()
    }
}

/// Conversions to/from libghostty's atoms, at the server's engine boundary.
#[cfg(feature = "server")]
mod libghostty_conv {
    use super::{KeyAction, ModSet, PhysicalKey};
    use libghostty_vt::key::{Action, Key, Mods};

    impl From<KeyAction> for Action {
        fn from(a: KeyAction) -> Self {
            match a {
                KeyAction::Release => Self::Release,
                KeyAction::Press => Self::Press,
                KeyAction::Repeat => Self::Repeat,
            }
        }
    }

    impl From<Action> for KeyAction {
        fn from(a: Action) -> Self {
            match a {
                Action::Release => Self::Release,
                Action::Repeat => Self::Repeat,
                // `Action` is #[non_exhaustive]; Press is the safe default.
                _ => Self::Press,
            }
        }
    }

    impl From<ModSet> for Mods {
        fn from(m: ModSet) -> Self {
            Self::from_bits_truncate(m.bits())
        }
    }

    impl From<Mods> for ModSet {
        fn from(m: Mods) -> Self {
            Self::from_bits_truncate(m.bits())
        }
    }

    impl From<PhysicalKey> for Key {
        fn from(k: PhysicalKey) -> Self {
            // Same discriminants (ADR-0024); unknown -> Unidentified.
            Self::try_from(k as u32).unwrap_or(Self::Unidentified)
        }
    }

    impl From<Key> for PhysicalKey {
        fn from(k: Key) -> Self {
            Self::try_from(k as u32).unwrap_or(Self::Unidentified)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_action_round_trips_wire_discriminant() {
        for a in [KeyAction::Release, KeyAction::Press, KeyAction::Repeat] {
            assert_eq!(KeyAction::from_u32(a.to_u32()), Some(a));
        }
        assert_eq!(KeyAction::from_u32(9), None);
    }

    #[test]
    fn key_event_equality_includes_text() {
        let mk = |text: &str| KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::A,
            mods: ModSet::CTRL | ModSet::SHIFT,
            consumed_mods: ModSet::SHIFT,
            composing: false,
            text: Some(text.to_owned()),
            unshifted_codepoint: None,
        };
        assert_ne!(mk("a"), mk("b"));
    }

    #[test]
    fn debug_redacts_key_text_and_codepoint() {
        let secret = "hunter2-PASSWORD";
        let event = KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::A,
            mods: ModSet::CTRL,
            consumed_mods: ModSet::empty(),
            composing: false,
            text: Some(secret.to_owned()),
            unshifted_codepoint: Some(u32::from('q')),
        };
        let rendered = format!("{event:?}");
        // The structural facts are present...
        assert!(rendered.contains("KeyEvent"), "{rendered}");
        assert!(rendered.contains("Press"), "{rendered}");
        assert!(rendered.contains('A'), "{rendered}");
        assert!(rendered.contains("text_len"), "{rendered}");
        // ...but the literal keystroke content is NOT.
        assert!(
            !rendered.contains(secret),
            "Debug leaked key text: {rendered}"
        );
        assert!(
            !rendered.contains("113") && !rendered.contains('q'),
            "Debug leaked unshifted codepoint: {rendered}"
        );
    }
}
