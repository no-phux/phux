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

use crate::schema::{Action, KeybindingsCfg};

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

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

/// One binding a lenient resolver build skipped, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingDiagnostic {
    /// The offending chord-sequence key, or the `prefix` string.
    pub binding: String,
    /// Why the binding was skipped.
    pub error: KeybindError,
}

impl std::fmt::Display for BindingDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "keybinding \"{}\": {}", self.binding, self.error)
    }
}

/// A resolver trie node: a leaf (action, no children) or an inner node
/// (children, no action); insertion forbids mixed nodes.
#[derive(Debug, Default, Clone)]
struct TrieNode {
    action: Option<ResolvedAction>,
    children: BTreeMap<KeyChord, Self>,
}

impl TrieNode {
    /// Insert a binding. Fails only on a node that already existed with
    /// conflicting content, before creating any fresh node.
    fn insert(
        &mut self,
        seq: &[KeyChord],
        action: ResolvedAction,
        full_text: &str,
    ) -> Result<(), KeybindError> {
        let ambiguous = || KeybindError::AmbiguousPrefix(full_text.to_owned());
        let Some((first, rest)) = seq.split_first() else {
            if self.action.is_some() || !self.children.is_empty() {
                return Err(ambiguous());
            }
            self.action = Some(action);
            return Ok(());
        };
        if self.action.is_some() {
            return Err(ambiguous());
        }
        self.children
            .entry(*first)
            .or_default()
            .insert(rest, action, full_text)
    }

    /// Parse and insert every binding of `table`, recording failures.
    fn insert_table(
        &mut self,
        table: &BTreeMap<String, Action>,
        diagnostics: &mut Vec<BindingDiagnostic>,
    ) {
        for (binding, action) in table {
            let action = ResolvedAction::from(action);
            let result = parse_chord_sequence(binding).and_then(|seq| {
                known_action(&action.action)?;
                self.insert(&seq, action, binding)
            });
            if let Err(error) = result {
                diagnostics.push(BindingDiagnostic {
                    binding: binding.clone(),
                    error,
                });
            }
        }
    }
}

/// Stateful keybind matcher: feed chords one at a time via
/// [`Resolver::feed`].
#[derive(Debug, Clone)]
pub struct Resolver {
    /// The trie; the prefix chord's subtree holds the prefix table.
    root: TrieNode,
    prefix: KeyChord,
    /// `None` at the root.
    cursor: Option<TrieNode>,
    /// Chords fed since the last reset (empty at the root).
    pending_path: Vec<KeyChord>,
}

/// Outcome of feeding one chord into a [`Resolver`].
#[derive(Debug, Clone, PartialEq)]
pub enum Feed {
    /// Nothing matched; the resolver is reset.
    NoMatch,
    /// The chord extended a partial sequence.
    Partial,
    /// The chord completed a binding; the resolver is reset.
    Resolved(ResolvedAction),
}

impl Resolver {
    /// Build a resolver, rejecting the table on its first problem.
    ///
    /// # Errors
    ///
    /// The first diagnostic [`Resolver::new_lenient`] reports.
    pub fn new(cfg: &KeybindingsCfg) -> Result<Self, KeybindError> {
        let (resolver, diagnostics) = Self::new_lenient(cfg);
        match diagnostics.into_iter().next() {
            None => Ok(resolver),
            Some(diag) => Err(diag.error),
        }
    }

    /// Build a resolver that degrades per binding, so one typo cannot
    /// disable every binding (including `detach`). In diagnostic order:
    ///
    /// 1. An unparseable `prefix` is replaced by the shipped default.
    /// 2. An unparseable binding is skipped.
    /// 3. A global binding at exactly the prefix chord is dropped (it could
    ///    never fire); the prefix table survives.
    /// 4. A binding naming an unknown action is skipped.
    /// 5. A binding ambiguous with an earlier one (in key order) is dropped.
    #[must_use]
    pub fn new_lenient(cfg: &KeybindingsCfg) -> (Self, Vec<BindingDiagnostic>) {
        let mut diagnostics = Vec::new();
        let prefix = parse_chord(&cfg.prefix).unwrap_or_else(|error| {
            diagnostics.push(BindingDiagnostic {
                binding: cfg.prefix.clone(),
                error,
            });
            KeyChord {
                modifiers: ModSet::CTRL,
                key: PhysicalKey::A,
            }
        });

        let mut root = TrieNode::default();
        root.insert_table(&cfg.global, &mut diagnostics);
        if !cfg.prefix_table.is_empty() {
            let prefix_child = root.children.entry(prefix).or_default();
            if prefix_child.action.take().is_some() {
                diagnostics.push(BindingDiagnostic {
                    binding: cfg.prefix.clone(),
                    error: KeybindError::AmbiguousPrefix(cfg.prefix.clone()),
                });
            }
            prefix_child.insert_table(&cfg.prefix_table, &mut diagnostics);
        }

        let resolver = Self {
            root,
            prefix,
            cursor: None,
            pending_path: Vec::new(),
        };
        (resolver, diagnostics)
    }

    /// Feed one chord. See [`Feed`] for outcome semantics.
    pub fn feed(&mut self, chord: KeyChord) -> Feed {
        let current = self.cursor.as_ref().unwrap_or(&self.root);
        let Some(next) = current.children.get(&chord) else {
            self.reset();
            return Feed::NoMatch;
        };
        if let Some(action) = &next.action {
            let resolved = action.clone();
            self.reset();
            return Feed::Resolved(resolved);
        }
        if next.children.is_empty() {
            self.reset();
            return Feed::NoMatch;
        }
        self.cursor = Some(next.clone());
        self.pending_path.push(chord);
        Feed::Partial
    }

    /// Clear any in-progress sequence and return to the root.
    pub fn reset(&mut self) {
        self.cursor = None;
        self.pending_path.clear();
    }

    /// `true` while a partial sequence is in flight.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.cursor.is_some()
    }

    /// `true` exactly when only the prefix chord has been pressed: the
    /// state the which-key popup describes.
    #[must_use]
    pub fn pending_at_prefix(&self) -> bool {
        self.pending_path.as_slice() == [self.prefix]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ck(s: &str) -> KeyChord {
        parse_chord(s).expect("parses")
    }

    fn cfg_from(toml: &str) -> crate::Config {
        crate::parse_str(toml, std::path::Path::new("test.toml")).expect("test config parses")
    }

    fn shipped() -> crate::Config {
        cfg_from(crate::DEFAULT_CONFIG_TOML)
    }

    fn resolves(resolver: &mut Resolver, chords: &[&str]) -> Option<ResolvedAction> {
        let (last, init) = chords.split_last()?;
        for chord in init {
            assert_eq!(resolver.feed(ck(chord)), Feed::Partial, "{chord}");
        }
        match resolver.feed(ck(last)) {
            Feed::Resolved(action) => Some(action),
            _ => None,
        }
    }

    /// Punctuation maps to US-layout physical keys; shifted glyphs carry
    /// Shift; non-ASCII stays unknown.
    #[test]
    fn punctuation_parses_to_physical_keys() {
        let none = ModSet::empty();
        for (spec, modifiers, key) in [
            ("|", ModSet::SHIFT, K::Backslash),
            ("S-\\", ModSet::SHIFT, K::Backslash),
            ("-", none, K::Minus),
            ("/", none, K::Slash),
            ("=", none, K::Equal),
            ("'", none, K::Quote),
            ("\"", ModSet::SHIFT, K::Quote),
            (";", none, K::Semicolon),
            ("C-/", ModSet::CTRL, K::Slash),
            ("F24", none, K::F24),
            ("7", none, K::Digit7),
        ] {
            assert_eq!(ck(spec), KeyChord { modifiers, key }, "{spec}");
        }
        for bad in ["→", "F0", "F25", "Ctrl-a", "C-"] {
            assert!(parse_chord(bad).is_err(), "{bad}");
        }
    }

    /// The shipped bindings build cleanly, bind no chord twice even under
    /// different spellings (`G` vs `S-g`), and keep the chords the TUI doc
    /// (§5.3) promises.
    #[test]
    fn shipped_keybindings_are_clean_and_documented() {
        let keys = shipped().keybindings;
        let mut resolver = Resolver::new(&keys).expect("no overlap");
        let prefix = ck(&keys.prefix);
        let rows = keys
            .global
            .keys()
            .map(|text| (Vec::new(), text))
            .chain(keys.prefix_table.keys().map(|text| (vec![prefix], text)));
        let mut seen: BTreeMap<Vec<KeyChord>, &String> = BTreeMap::new();
        for (mut seq, text) in rows {
            seq.extend(parse_chord_sequence(text).expect("default chord parses"));
            if let Some(earlier) = seen.insert(seq, text) {
                panic!("default bindings `{earlier}` and `{text}` share a chord");
            }
        }

        for (chord, action) in [
            ("s", "session-picker"),
            ("a", "session-picker"),
            ("w", "window-picker"),
            ("q", "next-attention"),
            ("Q", "return-from-attention"),
            ("A", "agent-fleet"),
            ("G", "go-to-directory"),
            ("F", "find-path"),
        ] {
            let got = resolves(&mut resolver, &["C-a", chord]).expect(chord);
            assert_eq!(got.action, action, "C-a {chord}");
        }
        for (chord, direction) in [("H", "left"), ("J", "down"), ("K", "up"), ("L", "right")] {
            let got = resolves(&mut resolver, &["C-a", chord]).expect(chord);
            assert_eq!(got.action, "resize-pane");
            assert_eq!(got.args.get("direction"), Some(&direction.into()));
            assert_eq!(got.args.get("amount"), Some(&5.into()));
        }
    }

    /// Each lenient-build degradation disables exactly the offending
    /// binding and keeps everything else reachable.
    #[test]
    fn lenient_build_skips_only_the_offending_binding() {
        let cases: &[(&str, &str, &[&str], &str)] = &[
            (
                "[keybindings.prefix-table]\n\"q-\" = \"kill-pane\"\nd = \"detach\"\n",
                "q-",
                &["C-a", "d"],
                "detach",
            ),
            (
                "[keybindings]\nprefix = \"Ctrl-a\"\n[keybindings.prefix-table]\nd = \"detach\"\n",
                "Ctrl-a",
                &["C-a", "d"],
                "detach",
            ),
            (
                "[keybindings.prefix-table]\n\"c\" = \"new-window\"\n\"c x\" = \"kill-pane\"\n",
                "c x",
                &["C-a", "c"],
                "new-window",
            ),
            (
                "[keybindings.global]\n\"C-a\" = \"kill-pane\"\n[keybindings.prefix-table]\nd = \"detach\"\n",
                "C-a",
                &["C-a", "d"],
                "detach",
            ),
            (
                "[keybindings.prefix-table]\nc = \"new-windoww\"\nd = \"detach\"\n",
                "c",
                &["C-a", "d"],
                "detach",
            ),
        ];
        for &(toml, bad, chords, action) in cases {
            let (mut resolver, diags) = Resolver::new_lenient(&cfg_from(toml).keybindings);
            let named: Vec<&str> = diags.iter().map(|d| d.binding.as_str()).collect();
            assert_eq!(named, [bad], "{toml}");
            assert_eq!(
                resolves(&mut resolver, chords).map(|a| a.action),
                Some(action.to_owned()),
                "{toml}"
            );
        }

        // The bad binding is dead, not misrouted.
        let cfg = cfg_from(cases[0].0);
        let (mut resolver, _) = Resolver::new_lenient(&cfg.keybindings);
        assert_eq!(resolver.feed(ck("C-a")), Feed::Partial);
        assert_eq!(resolver.feed(ck("q")), Feed::NoMatch);
    }

    /// A binding to an action no dispatcher knows is a diagnostic that names
    /// the nearest real action, so a strict build (reload) refuses it rather
    /// than installing a chord that silently does nothing.
    #[test]
    fn an_unknown_action_is_a_diagnostic_with_a_suggestion() {
        let cfg = cfg_from("[keybindings.prefix-table]\nc = \"new-windoww\"\n");
        let err = Resolver::new(&cfg.keybindings).expect_err("unknown action refused");
        assert_eq!(
            err.to_string(),
            "unknown action `new-windoww` (did you mean `new-window`?)"
        );
        let cfg = cfg_from("[keybindings.prefix-table]\nc = \"zzzzzzzzzz\"\n");
        let err = Resolver::new(&cfg.keybindings).expect_err("unknown action refused");
        assert_eq!(err.to_string(), "unknown action `zzzzzzzzzz`");
    }

    /// Strict `new` fails with exactly the first lenient diagnostic.
    #[test]
    fn strict_new_errors_with_the_first_lenient_diagnostic() {
        let cfg = cfg_from(
            "[keybindings]\nprefix = \"Ctrl-a\"\n[keybindings.prefix-table]\n\"q-\" = \"kill-pane\"\n",
        );
        let (_, diags) = Resolver::new_lenient(&cfg.keybindings);
        assert_eq!(diags.len(), 2);
        assert_eq!(Resolver::new(&cfg.keybindings).unwrap_err(), diags[0].error);
    }
}
