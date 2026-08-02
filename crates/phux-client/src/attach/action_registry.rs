//! Canonical action registry (phux-ahv.8).
//!
//! The command palette and the help overlay both need a human-facing
//! catalogue of the actions the dispatcher can run. That catalogue must
//! not drift from what [`run_action`](super::input_dispatch) actually
//! handles — a palette entry for an action the dispatcher ignores is a
//! dead command, and an action the dispatcher handles but the palette
//! omits is undiscoverable.
//!
//! ## Drift prevention
//!
//! There is one source of truth for the set of action *names*:
//! [`ACTION_NAMES`](super::input_dispatch::ACTION_NAMES), owned next to
//! `run_action`. This module's [`REGISTRY`] supplies the *presentation*
//! (description + the default [`ResolvedAction`] the palette commits) for
//! each of those names; [`NON_PALETTE_ACTIONS`] documents the remainder —
//! actions the dispatcher handles but the palette deliberately omits, each
//! with the reason. A unit test (`every_action_has_exactly_one_doc_home`)
//! asserts the two consts partition `ACTION_NAMES` exactly, so adding an
//! arm to `run_action` without documenting it — or vice versa — fails CI.
//! Adding a new action is therefore a three-touch change that the compiler
//! and the test funnel together: the `run_action` match arm, the
//! `ACTION_NAMES` entry, and the [`REGISTRY`] row (or
//! [`NON_PALETTE_ACTIONS`] entry). The generated reference page
//! `docs/reference/actions.md` renders from the union (see
//! `phux::refdocs::actions`), so the same funnel keeps the docs complete.
//!
//! Palette items resolve their *bound chord* at build time from the live
//! [`KeybindingsCfg`] snapshot, so the displayed shortcut always reflects
//! the user's actual config (or `"unbound"`).

use std::collections::BTreeMap;

use phux_config::keybind::ResolvedAction;
use phux_config::{Action, KeybindingsCfg};

use super::plugin_actions::PluginActionEntry;
use super::plugin_panes::PluginPaneEntry;
use crate::render::overlay::select_list::SelectItem;

/// The category a palette action groups under. Drives the dim section
/// headers the palette renders between groups; rows keep their category's
/// source order within a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Pane-level actions: split, kill, focus, resize, zoom, cycle.
    Pane,
    /// Window ("tab") actions: new/kill/cycle/rename/pick.
    Window,
    /// Session actions: new/rename/pick.
    Session,
    /// View / chrome actions: sidebar, help, detach.
    View,
}

impl Category {
    /// All categories in the order the palette renders their sections.
    const ORDER: &'static [Self] = &[Self::Pane, Self::Window, Self::Session, Self::View];

    /// The section-header label shown above this category's rows (also
    /// the palette-placement column of the generated actions reference).
    #[must_use]
    pub const fn header(self) -> &'static str {
        match self {
            Self::Pane => "Pane",
            Self::Window => "Window",
            Self::Session => "Session",
            Self::View => "View",
        }
    }
}

/// A registry row: an action the palette can offer.
#[derive(Debug, Clone, Copy)]
pub struct ActionSpec {
    /// Canonical action name (matches a `run_action` arm and an
    /// [`super::input_dispatch::ACTION_NAMES`] entry).
    pub name: &'static str,
    /// The section the palette groups this action under.
    pub category: Category,
    /// One-line human description shown in the palette.
    pub description: &'static str,
    /// The action's full parameter surface, for the generated reference
    /// page (`docs/reference/actions.md`): accepted keys with their value
    /// spaces, or `""` for a bare action. Documentation only — the
    /// dispatcher parses args itself; [`Self::args`] below is what the
    /// palette actually commits.
    pub params: &'static str,
    /// Inline `(key, value)` args the palette-committed
    /// [`ResolvedAction`] should carry. Empty for bare actions; e.g.
    /// `split-pane` carries `direction = "vertical"` so the palette
    /// commits a concrete, runnable action rather than a half-specified
    /// one that would bell.
    pub args: &'static [(&'static str, ArgValue)],
}

/// A statically-expressible argument value for a registry row.
///
/// `ResolvedAction::args` is a `BTreeMap<String, toml::Value>`, but
/// `toml::Value` isn't `const`-constructible, so the registry expresses
/// args with this small enum and converts at build time.
#[derive(Debug, Clone, Copy)]
pub enum ArgValue {
    /// A string-valued arg, e.g. `direction = "vertical"`.
    Str(&'static str),
    /// An integer-valued arg, e.g. `amount = 5`.
    Int(i64),
}

impl ArgValue {
    fn to_toml(self) -> toml::Value {
        match self {
            Self::Str(s) => toml::Value::String(s.to_owned()),
            Self::Int(n) => toml::Value::Integer(n),
        }
    }
}

impl ActionSpec {
    /// The [`ResolvedAction`] this spec commits when chosen from the
    /// palette — the same shape a keybinding produces, so it flows
    /// through `run_action` identically.
    #[must_use]
    pub fn resolved_action(&self) -> ResolvedAction {
        let mut args = BTreeMap::new();
        for (k, v) in self.args {
            args.insert((*k).to_owned(), v.to_toml());
        }
        ResolvedAction {
            action: self.name.to_owned(),
            args,
        }
    }
}

/// The canonical, in-tree catalogue of palette-offerable actions.
///
/// Every name here MUST be handled by a `run_action` arm and listed in
/// [`super::input_dispatch::ACTION_NAMES`] (enforced by a unit test).
///
/// Notes on inclusions/exclusions:
/// - `select-window` is parameterized by `index`, which the palette has
///   no UI to collect; the `<leader> w` window picker is the right
///   surface for "jump to window N", so it is omitted here.
/// - `rename-window` with no `name` arg opens the interactive prompt, so
///   the palette offers the bare form (prompt-driven).
/// - `command-palette` is omitted — opening the palette from the palette
///   is noise.
pub const REGISTRY: &[ActionSpec] = &[
    ActionSpec {
        name: "split-pane",
        category: Category::Pane,
        description: "Split the focused pane side-by-side (vertical divider)",
        params: "`direction` = `horizontal` | `vertical`",
        args: &[("direction", ArgValue::Str("vertical"))],
    },
    ActionSpec {
        name: "kill-pane",
        category: Category::Pane,
        description: "Close the focused pane",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "focus-direction",
        category: Category::Pane,
        description: "Move focus to the pane on the left",
        params: "`direction` = `left` | `right` | `up` | `down`",
        args: &[("direction", ArgValue::Str("left"))],
    },
    ActionSpec {
        name: "resize-pane",
        category: Category::Pane,
        description: "Grow the focused pane to the left",
        params: "`direction` = `left` | `right` | `up` | `down`; `amount` (cells)",
        args: &[
            ("direction", ArgValue::Str("left")),
            ("amount", ArgValue::Int(5)),
        ],
    },
    ActionSpec {
        name: "next-pane",
        category: Category::Pane,
        description: "Cycle focus to the next pane",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "next-attention",
        category: Category::Pane,
        description: "Jump to the next pane waiting for an answer",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "return-from-attention",
        category: Category::Pane,
        description: "Return to where attention navigation started",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "previous-pane",
        category: Category::Pane,
        description: "Cycle focus to the previous pane",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "last-pane",
        category: Category::Pane,
        description: "Jump back to the previously focused pane",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "toggle-zoom",
        category: Category::Pane,
        description: "Zoom the focused pane to fill the window (toggle)",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "context-menu",
        category: Category::Pane,
        description: "Open the context menu for the focused pane (ADR-0058)",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "new-window",
        category: Category::Window,
        description: "Open a new window",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "kill-window",
        category: Category::Window,
        description: "Close the active window and all its panes",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "next-window",
        category: Category::Window,
        description: "Switch to the next window",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "previous-window",
        category: Category::Window,
        description: "Switch to the previous window",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "window-picker",
        category: Category::Window,
        description: "Pick a window from all sessions (grouped)",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "rename-window",
        category: Category::Window,
        description: "Rename the active window (interactive prompt)",
        params: "`name?` (bare opens an interactive prompt)",
        args: &[],
    },
    ActionSpec {
        name: "session-picker",
        category: Category::Session,
        description: "Pick a session from a filterable list",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "new-session",
        category: Category::Session,
        description: "Create a new session and switch to it",
        params: "`name?` (bare opens an interactive prompt)",
        args: &[],
    },
    ActionSpec {
        name: "rename-session",
        category: Category::Session,
        description: "Rename the current session (interactive prompt)",
        params: "`name?` (bare opens an interactive prompt)",
        args: &[],
    },
    ActionSpec {
        name: "toggle-sidebar",
        category: Category::View,
        description: "Show or hide the window sidebar (toggle)",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "agent-fleet",
        category: Category::View,
        description: "Agent fleet: every pane's agent, state, and attention",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "show-help",
        category: Category::View,
        description: "Show the keybindings help overlay",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "detach",
        category: Category::View,
        description: "Detach this client from the session",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "reload-config",
        category: Category::View,
        description: "Reload the config file (keybindings, theme, status bar)",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "take-input",
        category: Category::Pane,
        description: "Take the wheel: seize exclusive input over the focused pane (ADR-0033)",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "give-input",
        category: Category::Pane,
        description: "Give back the wheel: release the focused pane's input lease (ADR-0033)",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "signal-terminal",
        category: Category::Pane,
        description: "Signal the focused pane's process group (freeze/resume/kill, ADR-0033)",
        params: "`signal` = `interrupt` | `freeze` | `resume` | `terminate` | `kill`",
        args: &[("signal", ArgValue::Str("freeze"))],
    },
    ActionSpec {
        name: "set-pane",
        category: Category::Pane,
        description: "Toggle per-pane mouse opt-out for the focused pane (ADR-0048)",
        params: "`mouse` = `on` | `off` | `toggle`",
        args: &[("mouse", ArgValue::Str("toggle"))],
    },
];

/// A dispatched action the palette deliberately does not offer.
///
/// Together with [`REGISTRY`], this const partitions
/// [`ACTION_NAMES`](phux_config::vocab::ACTION_NAMES): every dispatched
/// action has exactly one home — a palette row above, or an entry here
/// with the reason it has no row. The
/// `every_action_has_exactly_one_doc_home` test enforces the partition in
/// both directions, so adding (or removing) an action forces a doc blurb;
/// the generated `docs/reference/actions.md` renders from the union and
/// its freshness test forces the page regeneration.
#[derive(Debug, Clone, Copy)]
pub struct NonPaletteAction {
    /// Canonical action name (matches an
    /// [`ACTION_NAMES`](phux_config::vocab::ACTION_NAMES) entry).
    pub name: &'static str,
    /// One-line human description, same register as
    /// [`ActionSpec::description`].
    pub description: &'static str,
    /// Parameter surface, same register as [`ActionSpec::params`].
    pub params: &'static str,
    /// Why the palette has no row for it (surfaced in the generated
    /// reference so the omission reads as deliberate).
    pub reason: &'static str,
}

/// Dispatched-but-not-palette-offered actions, with the reason for each.
///
/// This is the single documented home for the palette exemptions the
/// lockstep test used to keep in a bare name list; the rationale for each
/// entry is unchanged from that list's comments.
pub const NON_PALETTE_ACTIONS: &[NonPaletteAction] = &[
    NonPaletteAction {
        name: "command-palette",
        description: "Open the command palette",
        params: "",
        reason: "opening the palette from the palette is noise",
    },
    NonPaletteAction {
        name: "select-window",
        description: "Focus the window at a given index",
        params: "`index` (0-based window position)",
        reason: "parameterized by `index`, which the palette has no UI to \
                 collect; the window picker is the surface for \"jump to \
                 window N\"",
    },
    NonPaletteAction {
        name: "switch-session",
        description: "Re-attach this client to another session",
        params: "`name`; `window?` (window index to select after the \
                 switch); `pane?` (DFS leaf ordinal to focus in that window)",
        reason: "requires a `name` arg supplied by the session picker (or \
                 the fleet's foreign rows), so a bare palette row would \
                 have no target to act on",
    },
    NonPaletteAction {
        name: "copy-mode",
        description: "Enter copy-mode on the focused pane (scrollback \
                      navigation, selection, yank)",
        params: "",
        reason: "a modal input surface entered from its keybinding, not a \
                 one-shot command the palette can commit",
    },
    NonPaletteAction {
        name: "plugin-action",
        description: "Run an enabled plugin's manifest action",
        params: "`plugin`, `action`",
        reason: "its palette rows are built dynamically from enabled \
                 plugins' manifests, one per manifest action, carrying \
                 `plugin`/`action` args a static row could not supply",
    },
    NonPaletteAction {
        name: "plugin-pane",
        description: "Open an enabled plugin's manifest pane",
        params: "`plugin`, `pane`",
        reason: "same shape as `plugin-action`: dynamic rows from enabled \
                 plugins' manifest `[[panes]]`, carrying `plugin`/`pane` \
                 args",
    },
    NonPaletteAction {
        name: "focus-pane",
        description: "Focus a pane by window index and DFS leaf ordinal",
        params: "`window` (window index), `pane` (DFS leaf ordinal)",
        reason: "parameterized by coordinates only the agent-fleet \
                 dashboard's rows can supply (the `select-window` \
                 precedent)",
    },
];

/// Build the palette's [`SelectItem`] rows from the [`REGISTRY`],
/// annotating each with its currently-bound chord (or `"unbound"`) and
/// grouping them under dim category headers ([`Category`]).
///
/// Rows are emitted category-by-category in [`Category`] order; each
/// non-empty category is preceded by a [`SelectItem::header`] section
/// label, and its action rows are [`indented`](SelectItem::indented) so the
/// grouping reads visually. The headers are non-selectable and disappear
/// once the user types a query (the filtered view is a flat best-first
/// ranking).
///
/// `keybindings` is the live config snapshot; `None` (config failed to
/// load) yields every row as `"unbound"`. The committed action is the
/// registry's [`ActionSpec::resolved_action`], so choosing a palette row
/// runs exactly what a keybinding would.
///
/// phux-r82.5: `plugin_actions` is the driver's snapshot of enabled
/// plugins' manifest `[[actions]]`. When non-empty, the rows follow the
/// static categories under a trailing **Plugin** header, labelled
/// `plugin: <plugin-name>: <action title>` and committing the shared
/// `plugin-action` dispatcher action (args `plugin`/`action`). These rows
/// are dynamic — they come from manifests, not [`REGISTRY`] — so they are
/// exempt from the registry↔dispatcher lockstep test (which pins the
/// `plugin-action` *name* instead; see `PALETTE_EXEMPT`). The bound-chord
/// annotation works unchanged because merged plugin keybindings carry the
/// same action + args shape (see
/// [`super::plugin_actions::merge_plugin_bindings`]).
///
/// phux-r82.7: `plugin_panes` is the driver's snapshot of enabled
/// plugins' hostable manifest `[[panes]]` (placement `split`/`tab`/
/// `zoomed`; overlay entries are dropped at snapshot time). Their rows
/// share the same trailing **Plugin** header, labelled
/// `plugin pane: <plugin-name>: <pane title>` and committing the
/// `plugin-pane` dispatcher action (args `plugin`/`pane`).
#[must_use]
pub fn palette_items(
    keybindings: Option<&KeybindingsCfg>,
    plugin_actions: &[PluginActionEntry],
    plugin_panes: &[PluginPaneEntry],
) -> Vec<SelectItem> {
    let mut items = Vec::new();
    for &category in Category::ORDER {
        let mut header_pushed = false;
        for spec in REGISTRY.iter().filter(|s| s.category == category) {
            if !header_pushed {
                items.push(SelectItem::header(category.header()));
                header_pushed = true;
            }
            let resolved = spec.resolved_action();
            items.push(
                SelectItem::new(spec.description, resolved.clone())
                    .secondary(chord_annotation(keybindings, &resolved))
                    .indented(),
            );
        }
    }
    let mut header_pushed = false;
    let plugin_rows = plugin_actions
        .iter()
        .map(|entry| (entry.palette_label(), entry.resolved_action()))
        .chain(
            plugin_panes
                .iter()
                .map(|entry| (entry.palette_label(), entry.resolved_action())),
        );
    for (label, resolved) in plugin_rows {
        if !header_pushed {
            items.push(SelectItem::header("Plugin"));
            header_pushed = true;
        }
        items.push(
            SelectItem::new(label, resolved.clone())
                .secondary(chord_annotation(keybindings, &resolved))
                .indented(),
        );
    }
    items
}

/// The chord annotation for a palette row: the bound chord's literal
/// keystrokes, or `"unbound"` (also when the config failed to load).
fn chord_annotation(keybindings: Option<&KeybindingsCfg>, resolved: &ResolvedAction) -> String {
    bound_chord_for(keybindings, resolved).unwrap_or_else(|| "unbound".to_owned())
}

/// The chord bound to `resolved`, or `None` when it is unbound (or the
/// config failed to load).
///
/// The palette renders `None` as the literal `"unbound"` because its rows
/// are a table with a shortcut column. The context menus (phux-wrnm) leave
/// an unbound row's annotation blank instead — a menu is not a reference
/// table, and a column of "unbound" reads as noise. Both go through this
/// one resolver so the two surfaces can never disagree about which chord
/// runs a row.
#[must_use]
pub fn bound_chord_for(
    keybindings: Option<&KeybindingsCfg>,
    resolved: &ResolvedAction,
) -> Option<String> {
    bound_chord(keybindings?, resolved)
}

/// Find the chord a user has bound to `target`, formatted as the literal
/// keystrokes to type.
///
/// Prefix-table entries are shown with the leader prefixed (e.g.
/// `"C-a |"`); global entries are shown as-is. The prefix table is
/// scanned before globals.
///
/// A registry action like `split-pane` may be bound under several chords
/// that differ only in args (`|` = vertical, `-` = horizontal). We prefer
/// the binding whose args exactly match the registry row, so the palette
/// shows the chord that runs *this* row; we fall back to a name-only
/// match when no exact-args binding exists. `None` when nothing maps to
/// the action name at all.
#[must_use]
fn bound_chord(cfg: &KeybindingsCfg, target: &ResolvedAction) -> Option<String> {
    // First pass: an exact (name + args) match.
    if let Some(chord) = scan(cfg, target, true) {
        return Some(chord);
    }
    // Fallback: any binding with the same action name.
    scan(cfg, target, false)
}

/// Scan the prefix table then globals for a binding to `target`'s action.
/// With `exact`, the binding's args must also equal `target.args`.
fn scan(cfg: &KeybindingsCfg, target: &ResolvedAction, exact: bool) -> Option<String> {
    for (chord, action) in &cfg.prefix_table {
        if binding_matches(action, target, exact) {
            return Some(format!("{} {chord}", cfg.prefix));
        }
    }
    for (chord, action) in &cfg.global {
        if binding_matches(action, target, exact) {
            return Some(chord.clone());
        }
    }
    None
}

/// `true` when `action` names `target.action` (and, when `exact`, its
/// resolved args equal `target.args`).
fn binding_matches(action: &Action, target: &ResolvedAction, exact: bool) -> bool {
    let resolved = ResolvedAction::from(action);
    resolved.action == target.action && (!exact || resolved.args == target.args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The glyphs of a VT byte stream: drop every CSI escape (the overlay paint
    /// emits SGR per styled cell and a CUP per row), leaving the text a user
    /// would read off the screen.
    fn strip_csi(vt: &str) -> String {
        let mut out = String::new();
        let mut chars = vt.chars();
        while let Some(c) = chars.next() {
            if c != '\u{1b}' {
                out.push(c);
                continue;
            }
            // Skip the `[` and the parameter bytes through the final byte
            // (0x40-0x7E). Only CSI sequences are emitted here.
            for esc in chars.by_ref() {
                if esc != '[' && ('@'..='~').contains(&esc) {
                    break;
                }
            }
        }
        out
    }

    /// phux-ep9s, end-to-end over the real registry: the full palette has more
    /// rows than a modal can show on any terminal, so the rows past the fold
    /// must be *reachable* — before the scroll viewport landed they were
    /// painted straight off the bottom edge of the box and the selection went
    /// with them.
    ///
    /// Drives the real overlay stack (`OverlayState::paint` → VT bytes) at a
    /// full-screen terminal size, not a synthetic list at a toy size.
    #[test]
    fn the_real_palette_scrolls_to_its_last_row() {
        use crate::render::Theme;
        use crate::render::overlay::{OverlayState, SelectList};
        use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

        let items = palette_items(None, &[], &[]);
        let last = items
            .iter()
            .rev()
            .find(|i| !i.is_header())
            .expect("the registry has at least one action")
            .label
            .clone();

        let mut overlays = OverlayState::new();
        overlays.push(Box::new(SelectList::new(
            "command palette",
            items,
            &Theme::default(),
        )));
        // A roomy terminal — the palette still overflows it, which is the
        // whole point: this is the geometry the bug was reported against.
        // The paint re-emits an SGR escape before every styled cell, so read
        // the glyphs the user actually sees, not the raw byte stream.
        let paint = |overlays: &OverlayState| {
            let mut out = Vec::new();
            overlays.paint(&mut out, (160, 48)).expect("paint");
            strip_csi(&String::from_utf8_lossy(&out))
        };

        let opened = paint(&overlays);
        assert!(
            !opened.contains(&last),
            "the real palette must overflow its modal for this test to prove \
             anything — `{last}` was expected below the fold",
        );
        assert!(
            opened.contains('█'),
            "an overflowing palette must paint a scrollbar so the user can see \
             there is more list:\n{opened:?}",
        );

        // End jumps to the last row: it must now be painted inside the box.
        overlays.handle_key(&KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::End,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        });
        let scrolled = paint(&overlays);
        assert!(
            scrolled.contains(&last),
            "the last action `{last}` must be reachable, not clipped away",
        );
    }

    /// The exhaustiveness gate (phux-i0e8.11.3): [`REGISTRY`] and
    /// [`NON_PALETTE_ACTIONS`] must partition `ACTION_NAMES` exactly —
    /// disjoint, and their union equal to the dispatched set in both
    /// directions. Adding a `run_action` arm therefore forces a described
    /// home (a palette row or a reasoned non-palette entry), which is what
    /// keeps the generated `docs/reference/actions.md` complete.
    #[test]
    fn every_action_has_exactly_one_doc_home() {
        let dispatched: BTreeSet<&str> = super::super::input_dispatch::ACTION_NAMES
            .iter()
            .copied()
            .collect();
        let registered: BTreeSet<&str> = REGISTRY.iter().map(|s| s.name).collect();
        let non_palette: BTreeSet<&str> = NON_PALETTE_ACTIONS.iter().map(|s| s.name).collect();

        // Disjoint: an action is palette-offered or reasoned-out, never both.
        if let Some(name) = registered.intersection(&non_palette).next() {
            panic!("`{name}` is both a REGISTRY row and a NON_PALETTE_ACTIONS entry");
        }

        // Every documented action is dispatched.
        for name in registered.union(&non_palette) {
            assert!(
                dispatched.contains(name),
                "`{name}` is documented but run_action has no arm (or ACTION_NAMES omits it)",
            );
        }
        // Every dispatched action is documented exactly once.
        for name in &dispatched {
            assert!(
                registered.contains(name) || non_palette.contains(name),
                "run_action handles `{name}` but it has no doc home \
                 (add a REGISTRY ActionSpec or a NON_PALETTE_ACTIONS entry)",
            );
        }
    }

    /// A non-palette entry's whole point is the blurb: every field that
    /// the generated reference renders must be non-empty (params may be
    /// empty — bare actions exist — but description and reason may not).
    #[test]
    fn non_palette_entries_carry_description_and_reason() {
        for spec in NON_PALETTE_ACTIONS {
            assert!(
                !spec.description.trim().is_empty(),
                "`{}` has an empty description",
                spec.name
            );
            assert!(
                !spec.reason.trim().is_empty(),
                "`{}` has an empty reason",
                spec.name
            );
        }
    }

    #[test]
    fn attention_navigation_actions_are_registered() {
        let names: BTreeSet<&str> = REGISTRY.iter().map(|spec| spec.name).collect();
        assert!(names.contains("next-attention"));
        assert!(names.contains("return-from-attention"));
    }

    #[test]
    fn resolved_action_carries_registry_args() {
        let split = REGISTRY
            .iter()
            .find(|s| s.name == "split-pane")
            .expect("split-pane registered");
        let ra = split.resolved_action();
        assert_eq!(ra.action, "split-pane");
        assert_eq!(
            ra.args.get("direction"),
            Some(&toml::Value::String("vertical".to_owned()))
        );
    }

    #[test]
    fn signal_terminal_palette_default_is_the_reversible_freeze() {
        // ADR-0033: signals are NOT lease-gated server-side, so the palette's
        // default arg is the safety boundary. It must stay the reversible
        // `freeze` (SIGSTOP) so a palette-dispatched signal-terminal can never
        // silently arm a destructive kill/terminate/interrupt.
        let sig = REGISTRY
            .iter()
            .find(|s| s.name == "signal-terminal")
            .expect("signal-terminal registered");
        assert_eq!(
            sig.resolved_action().args.get("signal"),
            Some(&toml::Value::String("freeze".to_owned())),
            "the palette default signal must remain the reversible freeze",
        );
    }

    #[test]
    fn palette_items_show_unbound_when_no_config() {
        let items = palette_items(None, &[], &[]);
        assert!(
            items
                .iter()
                .filter(|i| !i.is_header())
                .all(|i| i.secondary.as_deref() == Some("unbound")),
            "no config ⇒ every selectable row unbound",
        );
    }

    #[test]
    fn palette_items_group_under_category_headers() {
        let items = palette_items(None, &[], &[]);
        // Every category with members contributes exactly one header, in
        // ORDER, each immediately followed by indented action rows.
        let headers: Vec<&str> = items
            .iter()
            .filter(|i| i.is_header())
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(headers, vec!["Pane", "Window", "Session", "View"]);

        // Selectable rows are indented (nested under their header); headers
        // are not.
        for item in &items {
            if item.is_header() {
                assert!(!item.indented, "header `{}` must not indent", item.label);
            } else {
                assert!(item.indented, "row `{}` must indent", item.label);
            }
        }

        // The first row is a header (Pane), not a bare action.
        assert!(items[0].is_header(), "palette opens with a category header");
    }

    // ---------- phux-r82.5: dynamic plugin rows ----------

    fn plugin_entry(keys: Option<&str>) -> super::super::plugin_actions::PluginActionEntry {
        super::super::plugin_actions::PluginActionEntry {
            plugin_id: "com.example.tools".to_owned(),
            plugin_name: "Agent Tools".to_owned(),
            action_id: "summarize".to_owned(),
            title: "Summarize pane".to_owned(),
            keys: keys.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn plugin_actions_inject_namespaced_rows_under_plugin_header() {
        let items = palette_items(None, &[plugin_entry(None)], &[]);
        // The static categories are unchanged and the Plugin header trails.
        let headers: Vec<&str> = items
            .iter()
            .filter(|i| i.is_header())
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(headers, vec!["Pane", "Window", "Session", "View", "Plugin"]);

        let row = items
            .iter()
            .find(|i| !i.is_header() && i.label.starts_with("plugin: "))
            .expect("plugin row present");
        assert_eq!(row.label, "plugin: Agent Tools: Summarize pane");
        assert!(row.indented, "plugin rows nest under their header");
        // The committed action is the shared dispatcher action with the
        // plugin/action args — same shape a merged keybinding produces.
        assert_eq!(row.action.action, "plugin-action");
        assert_eq!(
            row.action.args.get("plugin"),
            Some(&toml::Value::String("com.example.tools".to_owned()))
        );
        assert_eq!(
            row.action.args.get("action"),
            Some(&toml::Value::String("summarize".to_owned()))
        );
    }

    #[test]
    fn no_plugin_actions_means_no_plugin_header() {
        let items = palette_items(None, &[], &[]);
        assert!(
            items.iter().all(|i| i.label != "Plugin"),
            "empty plugin snapshot must not add a Plugin section",
        );
    }

    #[test]
    fn plugin_row_shows_merged_binding_chord() {
        // Merge the plugin's `keys` into the prefix table the same way the
        // driver does, then confirm the palette annotates the row with the
        // literal keystrokes (prefix + chord).
        let entry = plugin_entry(Some("g"));
        let mut kb = KeybindingsCfg::default();
        super::super::plugin_actions::merge_plugin_bindings(&mut kb, std::slice::from_ref(&entry));
        let items = palette_items(Some(&kb), &[entry], &[]);
        let row = items
            .iter()
            .find(|i| i.label.starts_with("plugin: "))
            .expect("plugin row present");
        assert_eq!(row.secondary.as_deref(), Some("C-a g"));
    }

    // ---------- phux-r82.7: dynamic plugin pane rows ----------

    fn pane_entry() -> PluginPaneEntry {
        PluginPaneEntry {
            plugin_id: "com.example.tools".to_owned(),
            plugin_name: "Agent Tools".to_owned(),
            pane_id: "board".to_owned(),
            title: "Agent Board".to_owned(),
            placement: super::super::plugin_panes::HostedPlacement::Split,
            command: vec!["agent-board".to_owned()],
            plugin_root: std::path::PathBuf::from("/x"),
        }
    }

    #[test]
    fn plugin_panes_inject_namespaced_rows_under_shared_plugin_header() {
        let items = palette_items(None, &[plugin_entry(None)], &[pane_entry()]);
        // One shared Plugin header for actions and panes together.
        let headers: Vec<&str> = items
            .iter()
            .filter(|i| i.is_header())
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(headers, vec!["Pane", "Window", "Session", "View", "Plugin"]);

        let row = items
            .iter()
            .find(|i| !i.is_header() && i.label.starts_with("plugin pane: "))
            .expect("plugin pane row present");
        assert_eq!(row.label, "plugin pane: Agent Tools: Agent Board");
        assert!(row.indented, "plugin pane rows nest under their header");
        assert_eq!(row.action.action, "plugin-pane");
        assert_eq!(
            row.action.args.get("plugin"),
            Some(&toml::Value::String("com.example.tools".to_owned()))
        );
        assert_eq!(
            row.action.args.get("pane"),
            Some(&toml::Value::String("board".to_owned()))
        );
    }

    #[test]
    fn plugin_panes_alone_still_get_the_plugin_header() {
        let items = palette_items(None, &[], &[pane_entry()]);
        let headers: Vec<&str> = items
            .iter()
            .filter(|i| i.is_header())
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(headers, vec!["Pane", "Window", "Session", "View", "Plugin"]);
    }

    #[test]
    fn every_registry_action_has_a_category_in_order() {
        for spec in REGISTRY {
            assert!(
                Category::ORDER.contains(&spec.category),
                "`{}` has a category outside ORDER",
                spec.name,
            );
        }
    }
}
