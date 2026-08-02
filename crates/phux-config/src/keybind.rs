//! Keybind parser + resolver: matches `KeyEvent` sequences against
//! `[keybindings]` entries from the parsed [`Config`].
//!
//! # Surface
//!
//! [`parse_chord`] turns a single-chord string like `"C-c"`, `"M-S-Tab"`,
//! or `"F1"` into a [`KeyChord`]. [`parse_chord_sequence`] splits on
//! whitespace and parses each token. [`Resolver`] builds a trie over the
//! user's `[keybindings]` table and steps through it one [`KeyChord`] at
//! a time via [`Resolver::feed`].
//!
//! # Chord syntax
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
//! Single-character key tokens are canonicalized to lowercase. A bare
//! uppercase ASCII letter token (e.g. `"A"`) implies a `Shift` modifier
//! and a lowercase physical key — that is, `"A"` parses identically to
//! `"S-a"`. This keeps surface TOML readable (`"X"` for `Shift+x`) and
//! makes shifted-vs-unshifted bindings comparable as chord values.
//!
//! # Punctuation and the shifted-glyph rule
//!
//! ASCII punctuation parses as a single-char token mapping to the
//! corresponding physical key on a US layout: `-` → `Minus`, `=` →
//! `Equal`, `[` `]` → `BracketLeft` / `BracketRight`, `\` → `Backslash`,
//! `;` → `Semicolon`, `'` → `Quote`, `,` → `Comma`, `.` → `Period`, `/`
//! → `Slash`, `` ` `` → `Backquote`.
//!
//! Shifted punctuation glyphs (`!` `@` `#` `$` `%` `^` `&` `*` `(` `)`
//! `_` `+` `{` `}` `|` `:` `"` `<` `>` `?` `~`) map to their **unshifted
//! physical key plus an implicit `Shift` modifier**. For example `"|"`
//! parses identically to `"S-\\"` (`PhysicalKey::Backslash` + `SHIFT`),
//! and `"?"` parses as `"S-/"`. This is symmetric with the bare
//! uppercase letter rule above and matches what the wire protocol can
//! actually represent — wire chords carry physical keys, not glyphs, so
//! `|` and `Shift+\` are the same event downstream.
//!
//! The mapping assumes a US ANSI layout. Bindings written with shifted
//! glyphs on a non-US layout may not produce the chord the writer
//! expects; spell them with explicit modifiers (`S-<key>`) for layout
//! independence.
//!
//! [`Config`]: crate::Config

use std::collections::BTreeMap;

use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

use crate::schema::{Action, KeybindingsCfg};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors produced by chord parsing and resolver construction.
///
/// `Clone` + `PartialEq` so a lenient resolver build can carry the same
/// error value in a [`BindingDiagnostic`] that the strict build returns
/// (see [`Resolver::new`] / [`Resolver::new_lenient`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeybindError {
    /// Chord string was syntactically malformed (empty token, trailing
    /// dash, unrecognized modifier letter, ...). `pos` is a 0-based byte
    /// offset within the offending input.
    #[error("invalid chord syntax at {pos}: {message}")]
    Syntax {
        /// 0-based byte offset within the original input.
        pos: usize,
        /// Human-readable explanation.
        message: String,
    },

    /// Key token did not match any recognized named key or single
    /// character.
    #[error("unknown key name: {0}")]
    UnknownKey(String),

    /// A binding's full sequence is a strict prefix of another binding's
    /// sequence — the shorter would always resolve first, hiding the
    /// longer.
    #[error("ambiguous prefix: '{0}' is both a binding and a prefix")]
    AmbiguousPrefix(String),
}

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// A single modifier-key combination — one node in a [`KeybindSequence`].
///
/// `Ord`/`Hash` are implemented manually on top of the underlying numeric
/// representations of [`PhysicalKey`] (`#[repr(u32)]`) and [`ModSet`]
/// (`u16` bitflags). Neither upstream type derives `Hash` or `Ord`, but
/// they have well-defined numeric identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyChord {
    /// Modifier bitset (Ctrl/Shift/Alt/Super).
    pub modifiers: ModSet,
    /// The physical key.
    pub key: PhysicalKey,
}

/// The modifier bits a configured chord can carry. A live [`KeyEvent`]'s
/// `mods` may additionally report lock state (`CAPS_LOCK`, `NUM_LOCK`)
/// and left/right side bits (`SHIFT_SIDE`, ...) that chord strings never
/// spell. Matching masks `KeyEvent::mods` down to these four logical
/// modifiers so a live `Shift+x` reported with `SHIFT | SHIFT_SIDE`
/// still matches a chord parsed as `SHIFT`.
const CHORD_MODS: ModSet = ModSet::SHIFT
    .union(ModSet::CTRL)
    .union(ModSet::ALT)
    .union(ModSet::SUPER);

impl KeyChord {
    /// Does this configured chord match a live [`KeyEvent`]?
    ///
    /// Returns `true` when the event is a key *press* (not release or
    /// auto-repeat), targets the same [`PhysicalKey`], and carries the
    /// same logical modifiers once the event's `mods` are masked to
    /// the chord modifier set (lock and side bits ignored).
    ///
    /// This is the comparison the help-overlay dismiss path wants: feed
    /// it the chord parsed from the user's `show-help` binding and the
    /// live event, and dismiss on a match rather than on a hardcoded
    /// `F1`.
    #[must_use]
    pub fn matches_event(self, ev: &KeyEvent) -> bool {
        matches!(ev.action, KeyAction::Press)
            && ev.key == self.key
            && (ev.mods & CHORD_MODS) == self.modifiers
    }
}

impl std::hash::Hash for KeyChord {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.key as u32).hash(state);
        self.modifiers.bits().hash(state);
    }
}

impl PartialOrd for KeyChord {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for KeyChord {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // PhysicalKey is `#[repr(u32)]` so the cast is well-defined.
        let lk = self.key as u32;
        let rk = other.key as u32;
        lk.cmp(&rk)
            .then_with(|| self.modifiers.bits().cmp(&other.modifiers.bits()))
    }
}

/// A whitespace-separated list of [`KeyChord`]s, e.g. `C-b c`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeybindSequence(pub Vec<KeyChord>);

/// The result of a fully-matched binding: an action name plus any
/// inline-table parameters carried by [`Action::Parameterized`].
///
/// `Eq` is intentionally not derived: `toml::Value` is not `Eq` (it
/// carries `f64`). Use `PartialEq` for comparisons.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAction {
    /// Action name (e.g. `"new-pane"`, `"detach"`).
    pub action: String,
    /// Parameters from the source TOML inline table, or empty for
    /// [`Action::Bare`].
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

/// Parse a single chord string into a [`KeyChord`].
///
/// See the module documentation for the chord grammar.
pub fn parse_chord(s: &str) -> Result<KeyChord, KeybindError> {
    parse_chord_at(s, 0)
}

fn parse_chord_at(s: &str, base_pos: usize) -> Result<KeyChord, KeybindError> {
    if s.is_empty() {
        return Err(KeybindError::Syntax {
            pos: base_pos,
            message: "empty chord".to_owned(),
        });
    }

    let mut modifiers = ModSet::empty();
    let mut rest = s;
    let mut cursor = base_pos;

    // Peel modifier prefixes ("C-", "M-", "S-", "A-"). The final token is
    // the key — it may itself contain a `-` only via a named key (none of
    // our names do), so a trailing `-` after a modifier is an error.
    while let Some((head, tail)) = split_modifier(rest) {
        match head {
            'C' => modifiers |= ModSet::CTRL,
            'M' | 'A' => modifiers |= ModSet::ALT,
            'S' => modifiers |= ModSet::SHIFT,
            _ => {
                return Err(KeybindError::Syntax {
                    pos: cursor,
                    message: format!("unknown modifier '{head}'"),
                });
            }
        }
        cursor += 2;
        rest = tail;
    }

    if rest.is_empty() {
        return Err(KeybindError::Syntax {
            pos: cursor,
            message: "missing key after modifiers".to_owned(),
        });
    }

    // Reject stray modifier-like prefixes that weren't peeled (e.g. "C-")
    // — this branch should be unreachable since `split_modifier` only
    // returns Some when more text follows, but guard explicitly so the
    // contract is local.
    if rest.ends_with('-') && rest.len() > 1 {
        return Err(KeybindError::Syntax {
            pos: cursor + rest.len() - 1,
            message: "trailing dash".to_owned(),
        });
    }

    let (key, implicit_shift) = parse_key_token(rest)?;
    if implicit_shift {
        modifiers |= ModSet::SHIFT;
    }

    Ok(KeyChord { modifiers, key })
}

/// If `s` starts with `<letter>-` and there is more text after, return
/// `(<letter>, <rest_after_dash>)`. Otherwise `None`. Two-char modifier
/// tokens only — a longer prefix like `Ctrl-` is treated as a key name
/// candidate (and rejected later).
fn split_modifier(s: &str) -> Option<(char, &str)> {
    let mut chars = s.chars();
    let first = chars.next()?;
    if chars.next()? != '-' {
        return None;
    }
    let rest = &s[first.len_utf8() + 1..];
    if rest.is_empty() {
        return None;
    }
    // Modifier letters: C, M, S, A. Anything else is a key token (e.g.
    // `1-foo` would be nonsense, but treating non-modifier first chars
    // as key candidates lets `parse_key_token` produce the right error).
    if matches!(first, 'C' | 'M' | 'S' | 'A') {
        Some((first, rest))
    } else {
        None
    }
}

/// Parse the key portion of a chord. Returns `(key, implicit_shift)`
/// where `implicit_shift` is true when a bare uppercase ASCII letter or
/// a shifted ASCII punctuation glyph implied a Shift modifier.
fn parse_key_token(s: &str) -> Result<(PhysicalKey, bool), KeybindError> {
    // Single-character tokens: ASCII letter, digit, or punctuation.
    if s.chars().count() == 1 {
        let ch = s.chars().next().unwrap_or('\0');
        if ch.is_ascii_alphabetic() {
            let key = letter_to_key(ch.to_ascii_lowercase())
                .ok_or_else(|| KeybindError::UnknownKey(s.to_owned()))?;
            return Ok((key, ch.is_ascii_uppercase()));
        }
        if ch.is_ascii_digit() {
            let key = digit_to_key(ch).ok_or_else(|| KeybindError::UnknownKey(s.to_owned()))?;
            return Ok((key, false));
        }
        if let Some((key, implicit_shift)) = punct_to_key(ch) {
            return Ok((key, implicit_shift));
        }
        return Err(KeybindError::UnknownKey(s.to_owned()));
    }

    // Function keys: F1..F24.
    if let Some(rest) = s.strip_prefix('F')
        && let Ok(n) = rest.parse::<u8>()
        && let Some(key) = function_key(n)
    {
        return Ok((key, false));
    }

    let key = match s {
        // BackTab maps to Tab at the key level; line 293 sets implicit_shift
        // = true for BackTab so the chord carries Shift.
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
        _ => return Err(KeybindError::UnknownKey(s.to_owned())),
    };

    // `BackTab` is conventionally Shift+Tab.
    let implicit_shift = s == "BackTab";
    Ok((key, implicit_shift))
}

const fn letter_to_key(c: char) -> Option<PhysicalKey> {
    Some(match c {
        'a' => PhysicalKey::A,
        'b' => PhysicalKey::B,
        'c' => PhysicalKey::C,
        'd' => PhysicalKey::D,
        'e' => PhysicalKey::E,
        'f' => PhysicalKey::F,
        'g' => PhysicalKey::G,
        'h' => PhysicalKey::H,
        'i' => PhysicalKey::I,
        'j' => PhysicalKey::J,
        'k' => PhysicalKey::K,
        'l' => PhysicalKey::L,
        'm' => PhysicalKey::M,
        'n' => PhysicalKey::N,
        'o' => PhysicalKey::O,
        'p' => PhysicalKey::P,
        'q' => PhysicalKey::Q,
        'r' => PhysicalKey::R,
        's' => PhysicalKey::S,
        't' => PhysicalKey::T,
        'u' => PhysicalKey::U,
        'v' => PhysicalKey::V,
        'w' => PhysicalKey::W,
        'x' => PhysicalKey::X,
        'y' => PhysicalKey::Y,
        'z' => PhysicalKey::Z,
        _ => return None,
    })
}

const fn digit_to_key(c: char) -> Option<PhysicalKey> {
    Some(match c {
        '0' => PhysicalKey::Digit0,
        '1' => PhysicalKey::Digit1,
        '2' => PhysicalKey::Digit2,
        '3' => PhysicalKey::Digit3,
        '4' => PhysicalKey::Digit4,
        '5' => PhysicalKey::Digit5,
        '6' => PhysicalKey::Digit6,
        '7' => PhysicalKey::Digit7,
        '8' => PhysicalKey::Digit8,
        '9' => PhysicalKey::Digit9,
        _ => return None,
    })
}

/// Map an ASCII punctuation character to a physical key on the US layout.
///
/// Returns `(key, implicit_shift)`; shifted glyphs (e.g. `|`, `?`)
/// decompose into their unshifted physical key + `Shift`. See the
/// module rustdoc for the layout assumption.
///
/// `pub` so the client's stdin parser (`crates/phux-client/src/attach/input.rs`)
/// can share the same table — phux-gxy was caused by the input parser
/// mapping punctuation to `PhysicalKey::Unidentified` while the chord
/// parser used this table, so chords like `C-a |` (split-pane) never
/// matched.
#[must_use]
pub const fn punct_to_key(c: char) -> Option<(PhysicalKey, bool)> {
    Some(match c {
        // Unshifted glyphs.
        '`' => (PhysicalKey::Backquote, false),
        '-' => (PhysicalKey::Minus, false),
        '=' => (PhysicalKey::Equal, false),
        '[' => (PhysicalKey::BracketLeft, false),
        ']' => (PhysicalKey::BracketRight, false),
        '\\' => (PhysicalKey::Backslash, false),
        ';' => (PhysicalKey::Semicolon, false),
        '\'' => (PhysicalKey::Quote, false),
        ',' => (PhysicalKey::Comma, false),
        '.' => (PhysicalKey::Period, false),
        '/' => (PhysicalKey::Slash, false),
        // Shifted glyphs whose unshifted physical key is a digit.
        '!' => (PhysicalKey::Digit1, true),
        '@' => (PhysicalKey::Digit2, true),
        '#' => (PhysicalKey::Digit3, true),
        '$' => (PhysicalKey::Digit4, true),
        '%' => (PhysicalKey::Digit5, true),
        '^' => (PhysicalKey::Digit6, true),
        '&' => (PhysicalKey::Digit7, true),
        '*' => (PhysicalKey::Digit8, true),
        '(' => (PhysicalKey::Digit9, true),
        ')' => (PhysicalKey::Digit0, true),
        // Shifted glyphs whose unshifted physical key is punctuation.
        '~' => (PhysicalKey::Backquote, true),
        '_' => (PhysicalKey::Minus, true),
        '+' => (PhysicalKey::Equal, true),
        '{' => (PhysicalKey::BracketLeft, true),
        '}' => (PhysicalKey::BracketRight, true),
        '|' => (PhysicalKey::Backslash, true),
        ':' => (PhysicalKey::Semicolon, true),
        '"' => (PhysicalKey::Quote, true),
        '<' => (PhysicalKey::Comma, true),
        '>' => (PhysicalKey::Period, true),
        '?' => (PhysicalKey::Slash, true),
        _ => return None,
    })
}

const fn function_key(n: u8) -> Option<PhysicalKey> {
    Some(match n {
        1 => PhysicalKey::F1,
        2 => PhysicalKey::F2,
        3 => PhysicalKey::F3,
        4 => PhysicalKey::F4,
        5 => PhysicalKey::F5,
        6 => PhysicalKey::F6,
        7 => PhysicalKey::F7,
        8 => PhysicalKey::F8,
        9 => PhysicalKey::F9,
        10 => PhysicalKey::F10,
        11 => PhysicalKey::F11,
        12 => PhysicalKey::F12,
        13 => PhysicalKey::F13,
        14 => PhysicalKey::F14,
        15 => PhysicalKey::F15,
        16 => PhysicalKey::F16,
        17 => PhysicalKey::F17,
        18 => PhysicalKey::F18,
        19 => PhysicalKey::F19,
        20 => PhysicalKey::F20,
        21 => PhysicalKey::F21,
        22 => PhysicalKey::F22,
        23 => PhysicalKey::F23,
        24 => PhysicalKey::F24,
        _ => return None,
    })
}

/// Parse a whitespace-separated chord sequence into a
/// [`KeybindSequence`]. Empty input is an error.
pub fn parse_chord_sequence(s: &str) -> Result<KeybindSequence, KeybindError> {
    if s.trim().is_empty() {
        return Err(KeybindError::Syntax {
            pos: 0,
            message: "empty chord sequence".to_owned(),
        });
    }

    let mut chords = Vec::new();
    let mut cursor = 0_usize;
    for token in s.split_whitespace() {
        // Recover the byte offset of this token within `s` for accurate
        // error positions.
        let token_pos = s[cursor..].find(token).map_or(cursor, |off| cursor + off);
        chords.push(parse_chord_at(token, token_pos)?);
        cursor = token_pos + token.len();
    }
    Ok(KeybindSequence(chords))
}

/// Does a live [`KeyEvent`] match the chord spelled by `s`?
///
/// Convenience wrapper: parses `s` via [`parse_chord`] then delegates to
/// [`KeyChord::matches_event`]. A parse error yields `false` — a chord
/// string that doesn't parse can't match any event. Callers that need to
/// distinguish "no match" from "bad config" should parse once up front
/// with [`parse_chord`] and keep the [`KeyChord`].
#[must_use]
pub fn chord_str_matches_event(s: &str, ev: &KeyEvent) -> bool {
    parse_chord(s).is_ok_and(|chord| chord.matches_event(ev))
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

/// One binding skipped by a lenient resolver build
/// ([`Resolver::new_lenient`]): the binding text as written in config
/// plus the error that disabled it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingDiagnostic {
    /// The offending entry as written in config: a chord-sequence key
    /// from `[keybindings.global]` / `[keybindings.prefix-table]`, or
    /// the `prefix` string itself.
    pub binding: String,
    /// Why the binding was skipped.
    pub error: KeybindError,
}

impl std::fmt::Display for BindingDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "keybinding \"{}\": {}", self.binding, self.error)
    }
}

/// One node in the resolver trie. A node is either a leaf (carries a
/// [`ResolvedAction`] and has no children) or an inner node (children,
/// no action). The validator forbids mixed nodes (see
/// [`KeybindError::AmbiguousPrefix`]).
#[derive(Debug, Default, Clone)]
struct TrieNode {
    /// If `Some`, this node is a leaf binding.
    action: Option<ResolvedAction>,
    /// Children keyed by the chord that produces them.
    children: BTreeMap<KeyChord, Self>,
}

impl TrieNode {
    fn insert(
        &mut self,
        seq: &[KeyChord],
        action: ResolvedAction,
        full_text: &str,
    ) -> Result<(), KeybindError> {
        let Some((first, rest)) = seq.split_first() else {
            // Terminal: place action here. Must be empty and have no
            // existing leaf or children.
            if self.action.is_some() || !self.children.is_empty() {
                return Err(KeybindError::AmbiguousPrefix(full_text.to_owned()));
            }
            self.action = Some(action);
            return Ok(());
        };

        // Descending: this node must not itself be a leaf.
        if self.action.is_some() {
            return Err(KeybindError::AmbiguousPrefix(full_text.to_owned()));
        }

        let child = self.children.entry(*first).or_default();
        child.insert(rest, action, full_text)
    }
}

/// Stateful keybind matcher.
///
/// Build with [`Resolver::new`] from a [`KeybindingsCfg`], then feed
/// chords one at a time via [`Resolver::feed`]. Internally tracks an
/// in-progress sequence; resets on full match or mismatch.
#[derive(Debug, Clone)]
pub struct Resolver {
    /// The trie rooted at "no input yet". The prefix chord is one of its
    /// children whose subtree carries the prefix-table bindings.
    root: TrieNode,
    /// The parsed prefix chord. Compared against the pending path by
    /// [`Resolver::pending_at_prefix`] (the which-key popup trigger).
    prefix: KeyChord,
    /// Cursor: `None` means "at the root, waiting for the first chord".
    cursor: Option<TrieNode>,
    /// The chords fed since the last reset that led to the current
    /// partial state. Empty whenever `cursor` is `None`. Lets callers
    /// (the which-key popup) ask *which* pending state we are in
    /// without re-deriving it from raw key events.
    pending_path: Vec<KeyChord>,
}

/// Outcome of feeding one chord into a [`Resolver`].
#[derive(Debug, Clone, PartialEq)]
pub enum Feed {
    /// Chord didn't match anything from the current state. Resolver is
    /// reset.
    NoMatch,
    /// Chord extended a partial sequence — still waiting for the next
    /// chord.
    Partial,
    /// Chord completed a binding. Resolver is reset.
    Resolved(ResolvedAction),
}

impl Resolver {
    /// Construct a resolver from the user's `[keybindings]` table,
    /// rejecting the whole table on the first problem.
    ///
    /// Implemented on top of [`Resolver::new_lenient`] so the strict and
    /// lenient builds cannot diverge: this is exactly "build leniently,
    /// then fail with the first diagnostic if there was one".
    ///
    /// # Errors
    ///
    /// Returns [`KeybindError`] if any binding string fails to parse,
    /// the prefix string fails to parse, or two bindings form an
    /// ambiguous prefix relationship. The error equals the first
    /// diagnostic [`Resolver::new_lenient`] reports for the same table.
    pub fn new(cfg: &KeybindingsCfg) -> Result<Self, KeybindError> {
        let (resolver, diagnostics) = Self::new_lenient(cfg);
        match diagnostics.into_iter().next() {
            None => Ok(resolver),
            Some(diag) => Err(diag.error),
        }
    }

    /// Construct a resolver from the user's `[keybindings]` table,
    /// degrading **per binding**: every returned diagnostic disables
    /// exactly the binding it names, and every other binding keeps
    /// working (phux-i0e8.3.4 — one typo'd chord must not disable every
    /// binding including `detach`).
    ///
    /// Degradation policy, in check order (which fixes the diagnostic
    /// order, and therefore the error strict [`Resolver::new`] returns):
    ///
    /// 1. A `prefix` that fails to parse is reported and replaced by the
    ///    shipped default prefix, so the prefix table stays reachable.
    /// 2. A global or prefix-table binding whose chord sequence fails to
    ///    parse is reported and skipped.
    /// 3. A global binding at exactly the prefix chord (while a prefix
    ///    table exists) is ambiguous — it could never fire, because the
    ///    prefix chord must open the table — so it is reported and
    ///    dropped; the prefix table survives.
    /// 4. A binding that forms an ambiguous prefix with an
    ///    already-inserted one is reported and dropped (the earlier
    ///    binding, in `BTreeMap` key order, wins).
    #[must_use]
    pub fn new_lenient(cfg: &KeybindingsCfg) -> (Self, Vec<BindingDiagnostic>) {
        let mut diagnostics = Vec::new();

        let prefix = match parse_chord(&cfg.prefix) {
            Ok(chord) => chord,
            Err(error) => {
                diagnostics.push(BindingDiagnostic {
                    binding: cfg.prefix.clone(),
                    error,
                });
                // Fall back to the shipped default so the prefix table —
                // including any configured detach chord — stays reachable.
                // The default ("C-a") always parses; the literal fallback
                // is unreachable and only keeps this path panic-free.
                parse_chord(&KeybindingsCfg::default().prefix).unwrap_or(KeyChord {
                    modifiers: ModSet::CTRL,
                    key: PhysicalKey::A,
                })
            }
        };
        let mut root = TrieNode::default();

        // Global bindings: insert their parsed sequences directly under
        // the root. A failed insert never leaves stray trie nodes:
        // `TrieNode::insert` only fails on nodes that already existed
        // with conflicting content, before creating any fresh ones.
        for (binding, action) in &cfg.global {
            let seq = match parse_chord_sequence(binding) {
                Ok(seq) => seq,
                Err(error) => {
                    diagnostics.push(BindingDiagnostic {
                        binding: binding.clone(),
                        error,
                    });
                    continue;
                }
            };
            if let Err(error) = root.insert(&seq.0, ResolvedAction::from(action), binding) {
                diagnostics.push(BindingDiagnostic {
                    binding: binding.clone(),
                    error,
                });
            }
        }

        // Prefix-table bindings: insert under a single child keyed by
        // the prefix chord. Each table key is itself a (sub-)sequence,
        // so users can nest `"c x"` under the prefix if they want.
        if !cfg.prefix_table.is_empty() {
            let prefix_child = root.children.entry(prefix).or_default();
            // If a global binding terminated at exactly the prefix
            // chord, that node has an action set — and we're about to
            // nest children under it. That's ambiguous; the global leaf
            // loses (it could never fire anyway — the prefix chord must
            // open the table) so the whole prefix table survives.
            if prefix_child.action.take().is_some() {
                diagnostics.push(BindingDiagnostic {
                    binding: cfg.prefix.clone(),
                    error: KeybindError::AmbiguousPrefix(cfg.prefix.clone()),
                });
            }
            for (binding, action) in &cfg.prefix_table {
                let seq = match parse_chord_sequence(binding) {
                    Ok(seq) => seq,
                    Err(error) => {
                        diagnostics.push(BindingDiagnostic {
                            binding: binding.clone(),
                            error,
                        });
                        continue;
                    }
                };
                if let Err(error) =
                    prefix_child.insert(&seq.0, ResolvedAction::from(action), binding)
                {
                    diagnostics.push(BindingDiagnostic {
                        binding: binding.clone(),
                        error,
                    });
                }
            }
        }

        (
            Self {
                root,
                prefix,
                cursor: None,
                pending_path: Vec::new(),
            },
            diagnostics,
        )
    }

    /// Feed one chord. See [`Feed`] for outcome semantics.
    pub fn feed(&mut self, chord: KeyChord) -> Feed {
        // Lookup table: where do children come from?
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
            // Defensive: an inner node with no children and no action
            // would be a no-op leaf. Treat as no-match.
            self.reset();
            return Feed::NoMatch;
        }

        // Advance: clone the next node into the cursor. Trie nodes are
        // small and binding tables are short, so the clone is cheap.
        self.cursor = Some(next.clone());
        self.pending_path.push(chord);
        Feed::Partial
    }

    /// Clear any in-progress sequence and return to the root.
    pub fn reset(&mut self) {
        self.cursor = None;
        self.pending_path.clear();
    }

    /// `true` while a partial sequence is in flight (the last
    /// [`Resolver::feed`] returned [`Feed::Partial`] and no reset has
    /// happened since).
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.cursor.is_some()
    }

    /// `true` when the pending state is exactly "the prefix chord was
    /// pressed, awaiting a prefix-table continuation" — the state the
    /// which-key popup describes. `false` at the root, mid-way through
    /// a longer sequence (e.g. a nested `"c x"` prefix-table binding
    /// after `<prefix> c`), or mid-way through a multi-chord *global*
    /// binding that does not start with the prefix.
    #[must_use]
    pub fn pending_at_prefix(&self) -> bool {
        self.pending_path.as_slice() == [self.prefix]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ck(s: &str) -> KeyChord {
        parse_chord(s).expect("parses")
    }

    /// Build a key *press* event with the given physical key and mods and
    /// runtime-only fields zeroed — enough to exercise the matcher.
    fn press(key: PhysicalKey, mods: ModSet) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key,
            mods,
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        }
    }

    #[test]
    fn matches_bare_key() {
        // A bare key with no modifiers.
        let chord = ck("a");
        assert!(chord.matches_event(&press(PhysicalKey::A, ModSet::empty())));
        assert!(chord_str_matches_event(
            "a",
            &press(PhysicalKey::A, ModSet::empty())
        ));
        // Wrong key does not match.
        assert!(!chord.matches_event(&press(PhysicalKey::B, ModSet::empty())));
        // An unexpected modifier on the event does not match a bare chord.
        assert!(!chord.matches_event(&press(PhysicalKey::A, ModSet::CTRL)));
    }

    #[test]
    fn matches_modified_chord() {
        let chord = ck("C-a");
        assert!(chord.matches_event(&press(PhysicalKey::A, ModSet::CTRL)));
        // Same key without the modifier does not match.
        assert!(!chord.matches_event(&press(PhysicalKey::A, ModSet::empty())));
        // Extra logical modifier does not match.
        assert!(!chord.matches_event(&press(PhysicalKey::A, ModSet::CTRL | ModSet::SHIFT)));
    }

    #[test]
    fn matches_shift_shortcut() {
        // Bare uppercase letter implies Shift (see module docs), so the
        // chord carries SHIFT and must match a Shift-modified press.
        let chord = ck("A");
        assert_eq!(chord, ck("S-a"));
        assert!(chord.matches_event(&press(PhysicalKey::A, ModSet::SHIFT)));
        // Lock and side bits the OS may also report are ignored by the
        // mask, so a live Shift press still matches.
        let live = press(
            PhysicalKey::A,
            ModSet::SHIFT | ModSet::SHIFT_SIDE | ModSet::CAPS_LOCK,
        );
        assert!(chord.matches_event(&live));
    }

    #[test]
    fn matches_function_key() {
        let chord = ck("F1");
        assert!(chord.matches_event(&press(PhysicalKey::F1, ModSet::empty())));
        assert!(!chord.matches_event(&press(PhysicalKey::F2, ModSet::empty())));
    }

    #[test]
    fn does_not_match_non_press_actions() {
        let chord = ck("F1");
        let mut ev = press(PhysicalKey::F1, ModSet::empty());
        ev.action = KeyAction::Release;
        assert!(!chord.matches_event(&ev));
        ev.action = KeyAction::Repeat;
        assert!(!chord.matches_event(&ev));
    }

    #[test]
    fn chord_str_matches_event_is_false_on_parse_error() {
        // A malformed chord string can't match any event.
        assert!(!chord_str_matches_event(
            "C-",
            &press(PhysicalKey::A, ModSet::empty())
        ));
    }

    #[test]
    fn punct_unshifted_pipe_decomposes_to_shift_backslash() {
        // `|` is Shift+\ on US ANSI; per the module rustdoc it parses as
        // PhysicalKey::Backslash with an implicit Shift modifier, which
        // must match the explicit `S-\\` form.
        let bar = ck("|");
        let expl = ck("S-\\");
        assert_eq!(bar, expl);
        assert_eq!(
            bar,
            KeyChord {
                modifiers: ModSet::SHIFT,
                key: PhysicalKey::Backslash,
            }
        );
    }

    #[test]
    fn punct_minus_is_unshifted() {
        let c = ck("-");
        assert_eq!(
            c,
            KeyChord {
                modifiers: ModSet::empty(),
                key: PhysicalKey::Minus,
            }
        );
    }

    #[test]
    fn punct_slash_is_unshifted() {
        let c = ck("/");
        assert_eq!(
            c,
            KeyChord {
                modifiers: ModSet::empty(),
                key: PhysicalKey::Slash,
            }
        );
    }

    #[test]
    fn punct_equal_is_unshifted() {
        let c = ck("=");
        assert_eq!(
            c,
            KeyChord {
                modifiers: ModSet::empty(),
                key: PhysicalKey::Equal,
            }
        );
    }

    #[test]
    fn punct_apostrophe_is_unshifted_quote() {
        let c = ck("'");
        assert_eq!(
            c,
            KeyChord {
                modifiers: ModSet::empty(),
                key: PhysicalKey::Quote,
            }
        );
    }

    #[test]
    fn punct_double_quote_implies_shift_on_quote() {
        let c = ck("\"");
        assert_eq!(
            c,
            KeyChord {
                modifiers: ModSet::SHIFT,
                key: PhysicalKey::Quote,
            }
        );
    }

    #[test]
    fn ctrl_slash_combines_modifier_with_punctuation_key() {
        let c = ck("C-/");
        assert_eq!(
            c,
            KeyChord {
                modifiers: ModSet::CTRL,
                key: PhysicalKey::Slash,
            }
        );
    }

    #[test]
    fn punct_semicolon_is_unshifted() {
        let c = ck(";");
        assert_eq!(
            c,
            KeyChord {
                modifiers: ModSet::empty(),
                key: PhysicalKey::Semicolon,
            }
        );
    }

    #[test]
    fn non_ascii_char_is_still_unknown_key() {
        // ASCII punctuation now parses, but a non-ASCII codepoint (here a
        // BMP Unicode arrow) must still error — we only handle the US
        // ANSI glyph set.
        let err = parse_chord("→").unwrap_err();
        assert!(
            matches!(err, KeybindError::UnknownKey(ref s) if s == "→"),
            "got {err:?}"
        );
    }

    /// The shipped default config must have no overlapping or ambiguous
    /// keybindings: building a [`Resolver`] from it succeeds. Asserts the
    /// invariant, not which keys are bound — the defaults are free to change
    /// without touching this test.
    #[test]
    fn default_config_keybindings_have_no_overlap() {
        let cfg = crate::parse_str(
            crate::DEFAULT_CONFIG_TOML,
            std::path::Path::new("default.toml"),
        )
        .expect("default config parses");
        Resolver::new(&cfg.keybindings)
            .expect("default keybindings build a resolver with no overlap");
    }

    // -- lenient resolver build (phux-i0e8.3.4) ---------------------------

    /// Parse an inline `[keybindings]` TOML snippet into a full config.
    fn cfg_from(toml: &str) -> crate::Config {
        crate::parse_str(toml, std::path::Path::new("test.toml")).expect("test config parses")
    }

    #[test]
    fn lenient_build_skips_only_the_offending_binding() {
        // One malformed chord ("q-": trailing dash) among valid bindings,
        // including detach — the failure mode this exists for.
        let cfg = cfg_from(
            r#"
            [keybindings.prefix-table]
            "q-" = "kill-pane"
            d = "detach"
            x = "kill-pane"
            "#,
        );
        let (mut resolver, diags) = Resolver::new_lenient(&cfg.keybindings);
        assert_eq!(diags.len(), 1, "exactly the bad binding is reported");
        assert_eq!(diags[0].binding, "q-");
        assert!(matches!(diags[0].error, KeybindError::Syntax { .. }));

        // Every other binding still works — detach survives.
        assert_eq!(resolver.feed(ck("C-a")), Feed::Partial);
        match resolver.feed(ck("d")) {
            Feed::Resolved(ra) => assert_eq!(ra.action, "detach"),
            other => panic!("detach must survive one bad chord, got {other:?}"),
        }
        assert_eq!(resolver.feed(ck("C-a")), Feed::Partial);
        match resolver.feed(ck("x")) {
            Feed::Resolved(ra) => assert_eq!(ra.action, "kill-pane"),
            other => panic!("sibling binding must survive, got {other:?}"),
        }
        // The bad binding itself is dead, not misrouted.
        assert_eq!(resolver.feed(ck("C-a")), Feed::Partial);
        assert_eq!(resolver.feed(ck("q")), Feed::NoMatch);
    }

    #[test]
    fn lenient_build_falls_back_to_default_prefix_on_bad_prefix() {
        let cfg = cfg_from(
            r#"
            [keybindings]
            prefix = "Ctrl-a"

            [keybindings.prefix-table]
            d = "detach"
            "#,
        );
        let (mut resolver, diags) = Resolver::new_lenient(&cfg.keybindings);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].binding, "Ctrl-a");
        assert!(matches!(diags[0].error, KeybindError::UnknownKey(_)));

        // The prefix table hangs off the shipped default prefix instead.
        let default_prefix =
            parse_chord(&crate::KeybindingsCfg::default().prefix).expect("default prefix parses");
        assert_eq!(resolver.feed(default_prefix), Feed::Partial);
        assert!(resolver.pending_at_prefix());
        match resolver.feed(ck("d")) {
            Feed::Resolved(ra) => assert_eq!(ra.action, "detach"),
            other => panic!("prefix table must stay reachable, got {other:?}"),
        }
    }

    #[test]
    fn lenient_build_drops_the_later_of_two_ambiguous_bindings() {
        // "c" is a strict prefix of "c x"; BTreeMap order inserts "c"
        // first, so "c x" is the later binding and loses.
        let cfg = cfg_from(
            r#"
            [keybindings.prefix-table]
            "c" = "new-window"
            "c x" = "kill-pane"
            "#,
        );
        let (mut resolver, diags) = Resolver::new_lenient(&cfg.keybindings);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].binding, "c x");
        assert!(matches!(diags[0].error, KeybindError::AmbiguousPrefix(_)));

        // The earlier binding still resolves.
        assert_eq!(resolver.feed(ck("C-a")), Feed::Partial);
        match resolver.feed(ck("c")) {
            Feed::Resolved(ra) => assert_eq!(ra.action, "new-window"),
            other => panic!("earlier binding must win, got {other:?}"),
        }
    }

    #[test]
    fn lenient_build_drops_a_global_leaf_on_the_prefix_chord() {
        // A global binding at exactly the prefix chord could never fire
        // (the prefix chord must open the table); lenient drops it and
        // keeps the whole prefix table alive.
        let cfg = cfg_from(
            r#"
            [keybindings.global]
            "C-a" = "kill-pane"

            [keybindings.prefix-table]
            d = "detach"
            "#,
        );
        let (mut resolver, diags) = Resolver::new_lenient(&cfg.keybindings);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].binding, "C-a");
        assert!(matches!(diags[0].error, KeybindError::AmbiguousPrefix(_)));

        assert_eq!(
            resolver.feed(ck("C-a")),
            Feed::Partial,
            "prefix opens the table"
        );
        match resolver.feed(ck("d")) {
            Feed::Resolved(ra) => assert_eq!(ra.action, "detach"),
            other => panic!("prefix table must survive the conflict, got {other:?}"),
        }
    }

    #[test]
    fn strict_new_errors_with_the_first_lenient_diagnostic() {
        // Multiple problems: a bad prefix AND a bad binding. Strict `new`
        // must fail with exactly the first diagnostic's error, because it
        // is defined as "lenient build, then first diagnostic".
        let cfg = cfg_from(
            r#"
            [keybindings]
            prefix = "Ctrl-a"

            [keybindings.prefix-table]
            "q-" = "kill-pane"
            "#,
        );
        let (_, diags) = Resolver::new_lenient(&cfg.keybindings);
        assert_eq!(diags.len(), 2);
        let err = Resolver::new(&cfg.keybindings).expect_err("strict build must fail");
        assert_eq!(err, diags[0].error);
    }

    #[test]
    fn lenient_build_of_a_clean_table_reports_nothing() {
        let cfg = cfg_from(
            r#"
            [keybindings.prefix-table]
            d = "detach"
            "#,
        );
        let (_, diags) = Resolver::new_lenient(&cfg.keybindings);
        assert!(diags.is_empty(), "clean config must produce no diagnostics");
    }

    /// phux-foz.3: the default prefix table binds `H`/`J`/`K`/`L` to
    /// `resize-pane` left/down/up/right by 5 — the documented tmux-style
    /// resize row in the TUI doc's keybinding table (§5.3). Unlike the
    /// no-overlap test above, this pins the exact bindings: the doc table
    /// promises them, so changing them is a doc change too.
    #[test]
    fn default_config_binds_shift_hjkl_to_resize_pane() {
        let cfg = crate::parse_str(
            crate::DEFAULT_CONFIG_TOML,
            std::path::Path::new("default.toml"),
        )
        .expect("default config parses");
        for (chord, direction) in [("H", "left"), ("J", "down"), ("K", "up"), ("L", "right")] {
            let action = cfg
                .keybindings
                .prefix_table
                .get(chord)
                .unwrap_or_else(|| panic!("default prefix table binds `{chord}`"));
            let resolved = ResolvedAction::from(action);
            assert_eq!(resolved.action, "resize-pane", "chord `{chord}`");
            assert_eq!(
                resolved.args.get("direction"),
                Some(&toml::Value::String(direction.to_owned())),
                "chord `{chord}` resizes {direction}"
            );
            assert_eq!(
                resolved.args.get("amount"),
                Some(&toml::Value::Integer(5)),
                "chord `{chord}` resizes by the documented 5 cells"
            );
        }
    }
}
