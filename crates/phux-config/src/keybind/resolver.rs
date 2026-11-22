//! The resolver: a trie of the prefix, global, and key-mode tables, fed one
//! chord at a time, and the key-mode state machine on top of it.

use std::collections::BTreeMap;

use phux_protocol::input::key::{ModSet, PhysicalKey};

use super::{
    KeyChord, KeybindError, ResolvedAction, known_action, parse_chord, parse_chord_sequence,
};
use crate::schema::{Action, KeybindingsCfg};

/// The action that makes a key mode active: `{ action = "enter-mode",
/// name = "resize" }`, with `once = true` to leave after one binding.
pub const ENTER_MODE_ACTION: &str = "enter-mode";

/// The action that leaves the active key mode. Esc runs it in a mode that
/// does not bind Esc.
pub const EXIT_MODE_ACTION: &str = "exit-mode";

/// Whether `name` may name a key mode: lowercase ASCII letters, digits,
/// `-`, and `_`, so `mode:<name> <key>` and the TOML path read plainly.
#[must_use]
pub fn is_valid_mode_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// One binding a lenient resolver build skipped, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingDiagnostic {
    /// The offending chord-sequence key (`mode:<name> <key>` inside a key
    /// mode), or the `prefix` string.
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

/// Where one table's bindings go and what they may name.
struct TableCtx<'a> {
    /// `Some(name)` for a key mode's table: diagnostics read
    /// `mode:<name> <key>`.
    mode: Option<&'a str>,
    /// The key modes `enter-mode` may name.
    modes: &'a [&'a str],
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
        ctx: &TableCtx<'_>,
        diagnostics: &mut Vec<BindingDiagnostic>,
    ) {
        for (binding, action) in table {
            let label = ctx
                .mode
                .map_or_else(|| binding.clone(), |mode| format!("mode:{mode} {binding}"));
            let action = ResolvedAction::from(action);
            let result = parse_chord_sequence(binding).and_then(|seq| {
                known_action(&action.action)?;
                known_mode_target(&action, ctx.modes)?;
                self.insert(&seq, action, &label)
            });
            if let Err(error) = result {
                diagnostics.push(BindingDiagnostic {
                    binding: label,
                    error,
                });
            }
        }
    }
}

/// Refuse an `enter-mode` that names no defined mode.
fn known_mode_target(action: &ResolvedAction, modes: &[&str]) -> Result<(), KeybindError> {
    if action.action != ENTER_MODE_ACTION {
        return Ok(());
    }
    let Some(name) = action.args.get("name").and_then(toml::Value::as_str) else {
        return Err(KeybindError::MissingModeName);
    };
    if modes.contains(&name) {
        return Ok(());
    }
    Err(KeybindError::UnknownMode {
        name: name.to_owned(),
        suggestion: crate::vocab::did_you_mean(name, modes).map(str::to_owned),
    })
}

/// A key mode's bindings and options.
#[derive(Debug, Clone)]
struct ModeTable {
    root: TrieNode,
    exclusive: bool,
}

/// The key mode in effect.
#[derive(Debug, Clone)]
struct ActiveMode {
    name: String,
    /// Leave after the first of the mode's bindings fires.
    once: bool,
}

/// Stateful keybind matcher: feed chords one at a time via
/// [`Resolver::feed`].
#[derive(Debug, Clone)]
pub struct Resolver {
    /// The trie; the prefix chord's subtree holds the prefix table.
    root: TrieNode,
    prefix: KeyChord,
    /// Key modes by name.
    modes: BTreeMap<String, ModeTable>,
    /// The key mode in effect, if any.
    active: Option<ActiveMode>,
    /// `None` at the root.
    cursor: Option<TrieNode>,
    /// The pending sequence started in the active mode's table.
    cursor_in_mode: bool,
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
    /// An exclusive key mode is active and binds nothing for the chord: it
    /// goes nowhere. The resolver is reset.
    Swallowed,
}

/// Where one chord leads from a trie node.
enum Step {
    Miss,
    Leaf(ResolvedAction),
    Inner(TrieNode),
}

fn step(node: &TrieNode, chord: KeyChord) -> Step {
    let Some(next) = node.children.get(&chord) else {
        return Step::Miss;
    };
    match &next.action {
        Some(action) => Step::Leaf(action.clone()),
        None if next.children.is_empty() => Step::Miss,
        None => Step::Inner(next.clone()),
    }
}

/// Esc with no modifiers: the key that leaves a mode that does not bind it.
fn is_bare_escape(chord: KeyChord) -> bool {
    chord.key == PhysicalKey::Escape && chord.modifiers.is_empty()
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
    /// 4. A binding naming an unknown action, or an `enter-mode` naming no
    ///    defined mode, is skipped.
    /// 5. A binding ambiguous with an earlier one (in key order) is dropped.
    /// 6. A key mode with an invalid name is dropped whole; each mode's
    ///    bindings then follow rules 2, 4, and 5.
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
        let mode_names: Vec<&str> = cfg
            .modes
            .keys()
            .map(String::as_str)
            .filter(|name| is_valid_mode_name(name))
            .collect();
        let top = TableCtx {
            mode: None,
            modes: &mode_names,
        };

        let mut root = TrieNode::default();
        root.insert_table(&cfg.global, &top, &mut diagnostics);
        if !cfg.prefix_table.is_empty() {
            let prefix_child = root.children.entry(prefix).or_default();
            if prefix_child.action.take().is_some() {
                diagnostics.push(BindingDiagnostic {
                    binding: cfg.prefix.clone(),
                    error: KeybindError::AmbiguousPrefix(cfg.prefix.clone()),
                });
            }
            prefix_child.insert_table(&cfg.prefix_table, &top, &mut diagnostics);
        }

        let mut modes = BTreeMap::new();
        for (name, table) in &cfg.modes {
            if !is_valid_mode_name(name) {
                diagnostics.push(BindingDiagnostic {
                    binding: format!("mode:{name}"),
                    error: KeybindError::BadModeName(name.clone()),
                });
                continue;
            }
            let ctx = TableCtx {
                mode: Some(name),
                modes: &mode_names,
            };
            let mut mode_root = TrieNode::default();
            mode_root.insert_table(table, &ctx, &mut diagnostics);
            let exclusive = cfg.mode_options.get(name).is_some_and(|o| o.exclusive);
            modes.insert(
                name.clone(),
                ModeTable {
                    root: mode_root,
                    exclusive,
                },
            );
        }

        let resolver = Self {
            root,
            prefix,
            modes,
            active: None,
            cursor: None,
            cursor_in_mode: false,
            pending_path: Vec::new(),
        };
        (resolver, diagnostics)
    }

    /// Feed one chord. See [`Feed`] for outcome semantics.
    ///
    /// With a key mode active, a sequence starts in the mode's table. A
    /// chord the mode does not bind is swallowed in an exclusive mode; else
    /// it resolves as it would outside the mode, except that a bare Esc
    /// resolves to [`EXIT_MODE_ACTION`].
    pub fn feed(&mut self, chord: KeyChord) -> Feed {
        let exclusive = self.active_table().is_some_and(|mode| mode.exclusive);
        let outcome = match (&self.cursor, self.active_table()) {
            (Some(cursor), _) => step(cursor, chord),
            (None, Some(mode)) if mode.root.children.contains_key(&chord) => {
                let outcome = step(&mode.root, chord);
                self.cursor_in_mode = true;
                outcome
            }
            (None, Some(_)) if is_bare_escape(chord) => {
                return Feed::Resolved(ResolvedAction {
                    action: EXIT_MODE_ACTION.to_owned(),
                    args: BTreeMap::new(),
                });
            }
            (None, Some(_)) if exclusive => return Feed::Swallowed,
            (None, _) => step(&self.root, chord),
        };
        match outcome {
            Step::Miss => {
                self.reset();
                if exclusive {
                    Feed::Swallowed
                } else {
                    Feed::NoMatch
                }
            }
            Step::Leaf(action) => {
                let from_mode = self.cursor_in_mode;
                self.reset();
                if from_mode && self.active.as_ref().is_some_and(|mode| mode.once) {
                    self.active = None;
                }
                Feed::Resolved(action)
            }
            Step::Inner(node) => {
                self.cursor = Some(node);
                self.pending_path.push(chord);
                Feed::Partial
            }
        }
    }

    /// The active mode's table, if a mode is active.
    fn active_table(&self) -> Option<&ModeTable> {
        self.modes.get(&self.active.as_ref()?.name)
    }

    /// Make `name` the active key mode, replacing any other and dropping a
    /// pending sequence. `false` (and no change) when no such mode exists.
    pub fn enter_mode(&mut self, name: &str, once: bool) -> bool {
        if !self.modes.contains_key(name) {
            return false;
        }
        self.reset();
        self.active = Some(ActiveMode {
            name: name.to_owned(),
            once,
        });
        true
    }

    /// Leave the active key mode; `false` when none was active.
    pub fn exit_mode(&mut self) -> bool {
        self.reset();
        self.active.take().is_some()
    }

    /// The active key mode's name, if any.
    #[must_use]
    pub fn active_mode(&self) -> Option<&str> {
        self.active.as_ref().map(|mode| mode.name.as_str())
    }

    /// Clear any in-progress sequence and return to the root. The active
    /// key mode stays.
    pub fn reset(&mut self) {
        self.cursor = None;
        self.cursor_in_mode = false;
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
        !self.cursor_in_mode && self.pending_path.as_slice() == [self.prefix]
    }
}

#[cfg(test)]
mod tests;
