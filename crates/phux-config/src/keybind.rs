//! Keybind parser and resolver for the `[keybindings]` table.
//!
//! ```text
//! chord     := (modifier "-")* key
//! modifier  := "C" | "M" | "A" | "S"      // Ctrl, Meta, Alt (== Meta), Shift
//! key       := <single ASCII char> | <named key>
//! sequence  := chord (whitespace chord)*
//! ```
//!
//! Named keys (case-sensitive): `Tab`, `BackTab`, `Enter`, `Esc`, `Space`,
//! `Backspace`, `Delete`, `Up` / `Down` / `Left` / `Right`,
//! `Home` / `End` / `PageUp` / `PageDown`, `Insert`, `F1` ..= `F24`.
//!
//! Chords carry physical keys, not glyphs, so a bare uppercase letter means
//! Shift plus the letter (`"A"` == `"S-a"`), and a shifted punctuation glyph
//! means Shift plus its unshifted US-ANSI key (`"|"` == `"S-\\"`). On other
//! layouts, spell shifted bindings with explicit `S-`.

use std::collections::BTreeMap;

use phux_protocol::input::key::{ModSet, PhysicalKey, PhysicalKey as K};

use crate::schema::Action;

mod settle;
pub use settle::{UNBIND_ACTION, binding_layer, settle_bindings};

/// Errors produced by chord parsing and resolver construction.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeybindError {
    /// Malformed chord string; `pos` is a 0-based byte offset into it.
    #[error("invalid chord syntax at {pos}: {message}")]
    Syntax {
        /// 0-based byte offset within the original input.
        pos: usize,
        /// Human-readable explanation.
        message: String,
    },

    /// Key token matched no named key or single character.
    #[error("unknown key name: {0}")]
    UnknownKey(String),

    /// One binding's sequence is a strict prefix of another's, so the longer
    /// could never fire.
    #[error("ambiguous prefix: '{0}' is both a binding and a prefix")]
    AmbiguousPrefix(String),

    /// The binding names an action outside [`crate::vocab::ACTION_NAMES`]:
    /// no dispatcher runs it, so the chord would do nothing.
    #[error(
        "unknown action `{name}`{}",
        suggestion.map(|hit| format!(" (did you mean `{hit}`?)")).unwrap_or_default()
    )]
    UnknownAction {
        /// The action name as written.
        name: String,
        /// The closest known action, when one is near.
        suggestion: Option<&'static str>,
    },

    /// A `[keybindings.modes.<name>]` table whose name is not lowercase
    /// letters, digits, `-`, and `_`.
    #[error("invalid key mode name `{0}`: use lowercase letters, digits, `-`, and `_`")]
    BadModeName(String),

    /// `enter-mode` without a string `name`.
    #[error("enter-mode needs a mode: add name = \"<mode>\"")]
    MissingModeName,

    /// `enter-mode` names a mode no `[keybindings.modes.<name>]` table
    /// defines, so the key would do nothing.
    #[error(
        "unknown key mode `{name}`{}",
        suggestion.as_ref().map(|hit| format!(" (did you mean `{hit}`?)")).unwrap_or_default()
    )]
    UnknownMode {
        /// The mode name as written.
        name: String,
        /// The closest defined mode, when one is near.
        suggestion: Option<String>,
    },
}

/// Refuse an action name no dispatcher knows, naming the nearest known one.
fn known_action(name: &str) -> Result<(), KeybindError> {
    if crate::vocab::ACTION_NAMES.contains(&name) {
        return Ok(());
    }
    Err(KeybindError::UnknownAction {
        name: name.to_owned(),
        suggestion: crate::vocab::did_you_mean(name, crate::vocab::ACTION_NAMES),
    })
}

/// A single modifier-key combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyChord {
    /// Modifier bitset (Ctrl/Shift/Alt/Super).
    pub modifiers: ModSet,
    /// The physical key.
    pub key: PhysicalKey,
}

impl PartialOrd for KeyChord {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for KeyChord {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // PhysicalKey is `#[repr(u32)]`; neither upstream type derives `Ord`.
        (self.key as u32)
            .cmp(&(other.key as u32))
            .then_with(|| self.modifiers.bits().cmp(&other.modifiers.bits()))
    }
}

/// A fully-matched binding: the action name plus any inline-table args.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAction {
    /// Action name (e.g. `"new-pane"`, `"detach"`).
    pub action: String,
    /// Parameters from the source TOML inline table.
    pub args: BTreeMap<String, toml::Value>,
}

impl From<&Action> for ResolvedAction {
    fn from(action: &Action) -> Self {
        match action {
            Action::Bare(name) => Self {
                action: name.clone(),
                args: BTreeMap::new(),
            },
            Action::Parameterized(p) => Self {
                action: p.action.clone(),
                args: p.args.clone(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a single chord string (see the module grammar).
///
/// # Errors
///
/// [`KeybindError::Syntax`] or [`KeybindError::UnknownKey`].
pub fn parse_chord(s: &str) -> Result<KeyChord, KeybindError> {
    parse_chord_at(s, 0)
}

fn syntax(pos: usize, message: &str) -> KeybindError {
    KeybindError::Syntax {
        pos,
        message: message.to_owned(),
    }
}

fn parse_chord_at(s: &str, base_pos: usize) -> Result<KeyChord, KeybindError> {
    if s.is_empty() {
        return Err(syntax(base_pos, "empty chord"));
    }

    let mut modifiers = ModSet::empty();
    let mut rest = s;
    let mut cursor = base_pos;
    while let Some((head, tail)) = split_modifier(rest) {
        modifiers |= match head {
            'C' => ModSet::CTRL,
            'S' => ModSet::SHIFT,
            _ => ModSet::ALT, // 'M' | 'A'
        };
        cursor += 2;
        rest = tail;
    }
    if rest.len() > 1 && rest.ends_with('-') {
        return Err(syntax(cursor + rest.len() - 1, "trailing dash"));
    }

    let (key, implicit_shift) = parse_key_token(rest)?;
    if implicit_shift {
        modifiers |= ModSet::SHIFT;
    }
    Ok(KeyChord { modifiers, key })
}

/// Split `<C|M|S|A>-<more>` into the modifier letter and the rest.
fn split_modifier(s: &str) -> Option<(char, &str)> {
    let rest = s.get(2..).filter(|rest| !rest.is_empty())?;
    let mut chars = s.chars();
    let first = chars.next()?;
    (matches!(first, 'C' | 'M' | 'S' | 'A') && chars.next()? == '-').then_some((first, rest))
}

/// The key portion of a chord, and whether it implies Shift.
fn parse_key_token(s: &str) -> Result<(PhysicalKey, bool), KeybindError> {
    let unknown = || KeybindError::UnknownKey(s.to_owned());
    let mut chars = s.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        return single_char_key(ch).ok_or_else(unknown);
    }

    if let Some(n) = s.strip_prefix('F').and_then(|n| n.parse::<usize>().ok())
        && let Some(&key) = n.checked_sub(1).and_then(|i| FUNCTION_KEYS.get(i))
    {
        return Ok((key, false));
    }

    let key = match s {
        "Tab" | "BackTab" => PhysicalKey::Tab,
        "Enter" => PhysicalKey::Enter,
        "Esc" | "Escape" => PhysicalKey::Escape,
        "Space" => PhysicalKey::Space,
        "Backspace" => PhysicalKey::Backspace,
        "Delete" => PhysicalKey::Delete,
        "Up" => PhysicalKey::ArrowUp,
        "Down" => PhysicalKey::ArrowDown,
        "Left" => PhysicalKey::ArrowLeft,
        "Right" => PhysicalKey::ArrowRight,
        "Home" => PhysicalKey::Home,
        "End" => PhysicalKey::End,
        "PageUp" => PhysicalKey::PageUp,
        "PageDown" => PhysicalKey::PageDown,
        "Insert" => PhysicalKey::Insert,
        _ => return Err(unknown()),
    };
    Ok((key, s == "BackTab"))
}

const LETTERS: [PhysicalKey; 26] = [
    K::A,
    K::B,
    K::C,
    K::D,
    K::E,
    K::F,
    K::G,
    K::H,
    K::I,
    K::J,
    K::K,
    K::L,
    K::M,
    K::N,
    K::O,
    K::P,
    K::Q,
    K::R,
    K::S,
    K::T,
    K::U,
    K::V,
    K::W,
    K::X,
    K::Y,
    K::Z,
];

const DIGITS: [PhysicalKey; 10] = [
    K::Digit0,
    K::Digit1,
    K::Digit2,
    K::Digit3,
    K::Digit4,
    K::Digit5,
    K::Digit6,
    K::Digit7,
    K::Digit8,
    K::Digit9,
];

const FUNCTION_KEYS: [PhysicalKey; 24] = [
    K::F1,
    K::F2,
    K::F3,
    K::F4,
    K::F5,
    K::F6,
    K::F7,
    K::F8,
    K::F9,
    K::F10,
    K::F11,
    K::F12,
    K::F13,
    K::F14,
    K::F15,
    K::F16,
    K::F17,
    K::F18,
    K::F19,
    K::F20,
    K::F21,
    K::F22,
    K::F23,
    K::F24,
];

fn single_char_key(ch: char) -> Option<(PhysicalKey, bool)> {
    if ch.is_ascii_alphabetic() {
        let index = usize::from(ch.to_ascii_lowercase() as u8 - b'a');
        return Some((LETTERS[index], ch.is_ascii_uppercase()));
    }
    if let Some(digit) = ch.to_digit(10) {
        return DIGITS.get(digit as usize).map(|&key| (key, false));
    }
    punct_to_key(ch)
}

/// Map ASCII punctuation to its US-layout physical key and whether it
/// implies Shift (`|` is Shift + `\`). Shared with the client's stdin parser
/// so chord matching and input parsing agree.
#[must_use]
pub const fn punct_to_key(c: char) -> Option<(PhysicalKey, bool)> {
    Some(match c {
        '`' => (K::Backquote, false),
        '-' => (K::Minus, false),
        '=' => (K::Equal, false),
        '[' => (K::BracketLeft, false),
        ']' => (K::BracketRight, false),
        '\\' => (K::Backslash, false),
        ';' => (K::Semicolon, false),
        '\'' => (K::Quote, false),
        ',' => (K::Comma, false),
        '.' => (K::Period, false),
        '/' => (K::Slash, false),
        '!' => (K::Digit1, true),
        '@' => (K::Digit2, true),
        '#' => (K::Digit3, true),
        '$' => (K::Digit4, true),
        '%' => (K::Digit5, true),
        '^' => (K::Digit6, true),
        '&' => (K::Digit7, true),
        '*' => (K::Digit8, true),
        '(' => (K::Digit9, true),
        ')' => (K::Digit0, true),
        '~' => (K::Backquote, true),
        '_' => (K::Minus, true),
        '+' => (K::Equal, true),
        '{' => (K::BracketLeft, true),
        '}' => (K::BracketRight, true),
        '|' => (K::Backslash, true),
        ':' => (K::Semicolon, true),
        '"' => (K::Quote, true),
        '<' => (K::Comma, true),
        '>' => (K::Period, true),
        '?' => (K::Slash, true),
        _ => return None,
    })
}

/// Parse a whitespace-separated chord sequence. Empty input is an error.
///
/// # Errors
///
/// [`KeybindError::Syntax`] or [`KeybindError::UnknownKey`], positioned
/// within `s`.
pub fn parse_chord_sequence(s: &str) -> Result<Vec<KeyChord>, KeybindError> {
    if s.trim().is_empty() {
        return Err(syntax(0, "empty chord sequence"));
    }
    let mut chords = Vec::new();
    let mut cursor = 0_usize;
    for token in s.split_whitespace() {
        let token_pos = s[cursor..].find(token).map_or(cursor, |off| cursor + off);
        chords.push(parse_chord_at(token, token_pos)?);
        cursor = token_pos + token.len();
    }
    Ok(chords)
}

mod resolver;
pub use resolver::{
    BindingDiagnostic, ENTER_MODE_ACTION, EXIT_MODE_ACTION, Feed, Resolver, is_valid_mode_name,
};

#[cfg(test)]
mod tests;
