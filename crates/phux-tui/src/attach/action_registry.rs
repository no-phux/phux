//! Canonical action registry: the palette's human-facing catalogue of the
//! actions `run_action` handles.
//!
//! [`ACTION_NAMES`](phux_config::vocab::ACTION_NAMES) is the one list of
//! names. [`REGISTRY`] gives each palette-offered action its presentation;
//! [`NON_PALETTE_ACTIONS`] gives every other one the reason it has no row. A
//! test asserts the two partition `ACTION_NAMES` exactly, and the generated
//! `docs/reference/actions.md` renders from their union. Bound chords resolve
//! from the live [`KeybindingsCfg`] at build time.

use std::collections::BTreeMap;

use phux_config::keybind::ResolvedAction;
use phux_config::{Action, KeybindingsCfg};

use super::plugin_actions::PluginActionEntry;
use super::plugin_panes::PluginPaneEntry;
use crate::render::overlay::select_list::SelectItem;

/// The section a palette action groups under, in palette order.
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
    /// [`phux_config::vocab::ACTION_NAMES`] entry).
    pub name: &'static str,
    /// The section the palette groups this action under.
    pub category: Category,
    /// One-line human description shown in the palette.
    pub description: &'static str,
    /// The parameter surface for the generated reference page (`""` for a
    /// bare action); documentation only.
    pub params: &'static str,
    /// Inline args the palette-committed action carries, so a row commits a
    /// runnable action rather than a half-specified one that would bell.
    pub args: &'static [(&'static str, ArgValue)],
}

/// A `const`-constructible argument value (`toml::Value` is not).
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
    /// The [`ResolvedAction`] this row commits, the same shape a keybinding
    /// produces.
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

/// The palette-offerable actions. Every name must have a `run_action` arm
/// and an `ACTION_NAMES` entry (enforced by a test).
pub const REGISTRY: &[ActionSpec] = &[
    ActionSpec {
        name: "split-pane",
        category: Category::Pane,
        description: "Split the focused pane side-by-side (vertical divider)",
        params: "`direction` = `horizontal` | `vertical`; `host?` (satellite); `resource?` (`@N` or `host/@N`)",
        args: &[("direction", ArgValue::Str("vertical"))],
    },
    ActionSpec {
        name: "move-pane",
        category: Category::Pane,
        description: "Move the focused pane beside another pane…",
        params: "`target` (local Terminal id; picker-supplied)",
        args: &[],
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
        description: "Open the context menu for the focused pane",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "new-window",
        category: Category::Window,
        description: "Open a new window",
        params: "`cwd?` (working directory on the attached server's host)",
        args: &[],
    },
    ActionSpec {
        name: "go-to-directory",
        category: Category::Window,
        description: "Browse directories on the attached server's host and open a new window in one",
        params: "`path?` (absolute, `~`, or `~/...`; bare starts at the focused pane's directory)",
        args: &[],
    },
    ActionSpec {
        name: "find-path",
        category: Category::Pane,
        description: "Browse or fuzzy-search host paths and insert a literal path into the focused pane",
        params: "`path?` (directory to browse; defaults to the focused pane's directory)",
        args: &[],
    },
    ActionSpec {
        name: "insert-path",
        category: Category::Pane,
        description: "Insert a selected host path without running the command",
        params: "`path` (selected absolute path)",
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
        description: "Browse sessions and live host availability",
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
        name: "settings",
        category: Category::View,
        description: "Settings: browse, search, and edit every option in place",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "report-bug",
        category: Category::View,
        description: "Write a local bug report (session, logs, screen) and copy its path",
        params: "`note?` (optional free-text description)",
        args: &[],
    },
    ActionSpec {
        name: "getting-started",
        category: Category::View,
        description: "Getting started: detach, return, the Agents list, and command discovery",
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
        description: "Take the wheel: seize exclusive input over the focused pane",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "give-input",
        category: Category::Pane,
        description: "Give back the wheel: release the focused pane's input lease",
        params: "",
        args: &[],
    },
    ActionSpec {
        name: "signal-terminal",
        category: Category::Pane,
        description: "Signal the focused pane's process group (freeze, resume, kill)",
        params: "`signal` = `interrupt` | `freeze` | `resume` | `terminate` | `kill`",
        args: &[("signal", ArgValue::Str("freeze"))],
    },
    ActionSpec {
        name: "set-pane",
        category: Category::Pane,
        description: "Toggle mouse capture for the focused pane",
        params: "`mouse` = `on` | `off` | `toggle`",
        args: &[("mouse", ArgValue::Str("toggle"))],
    },
];

/// A dispatched action the palette deliberately does not offer, with the
/// reason (rendered in the generated reference).
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

/// Dispatched-but-not-palette-offered actions.
pub const NON_PALETTE_ACTIONS: &[NonPaletteAction] = &[
    NonPaletteAction {
        name: "command-palette",
        description: "Open the fuzzy commands and help finder",
        params: "",
        reason: "it is an entry alias for the finder, so listing it inside the finder would recurse",
    },
    NonPaletteAction {
        name: "show-help",
        description: "Open the fuzzy commands and help finder",
        params: "",
        reason: "it is an entry alias for the same finder as `command-palette`, so listing it would duplicate that surface",
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
        name: "move-window",
        description: "Move the active window to another position in the window order",
        params: "`index` (0-based destination) or `delta` (signed slots, e.g. `-1`); \
                 clamped to the ends",
        reason: "parameterized by direction; bound to `<` and `>` under the leader, \
                 offered in the window context menu, and done by dragging a tab \
                 or a sidebar window row",
    },
    NonPaletteAction {
        name: "switch-session",
        description: "Re-attach this client to another session",
        params: "`name`; `window?` (window index to select after the \
                 switch); `pane?` (DFS leaf ordinal to focus in that \
                 window); `host?` (a satellite of this hub: opens that \
                 session's active pane here through the relay instead of \
                 re-attaching)",
        reason: "requires a `name` arg supplied by the session picker (or \
                 the fleet's foreign rows), so a bare palette row would \
                 have no target to act on",
    },
    NonPaletteAction {
        name: "switch-host",
        description: "Re-attach this terminal to a session on another machine",
        params: "`host` (a registered host name, or `local` for this \
                 machine); `name` (the session there)",
        reason: "requires `host` and `name` args supplied by the sidebar's \
                 machine segments, so a bare palette row would have no \
                 target to act on",
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

/// The palette rows.
///
/// [`REGISTRY`] grouped under [`Category`] headers, each row annotated by its
/// bound chord (`"unbound"` without one or without a config), then enabled
/// plugins' actions and panes under a trailing **Plugin** header.
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

/// The chord bound to `resolved`, or `None`. The palette renders `None` as
/// `"unbound"`; context menus leave it blank. Both use this one resolver so
/// they never disagree.
#[must_use]
pub fn bound_chord_for(
    keybindings: Option<&KeybindingsCfg>,
    resolved: &ResolvedAction,
) -> Option<String> {
    bound_chord(keybindings?, resolved)
}

/// The chord bound to `target` as literal keystrokes (prefix-table entries
/// with the leader). Prefers a binding whose args match exactly (`|` vs `-`
/// for `split-pane`), then any binding of the same action name.
#[must_use]
fn bound_chord(cfg: &KeybindingsCfg, target: &ResolvedAction) -> Option<String> {
    // First pass: an exact (name + args) match.
    if let Some(chord) = scan(cfg, target, true) {
        return Some(chord);
    }
    // Fallback: any binding with the same action name.
    scan(cfg, target, false)
}

/// Canonical chords shown ahead of a shipped compatibility alias, unless the
/// user removed or rebound them.
const PRIMARY_PREFIX_BINDINGS: &[(&str, &str)] = &[("session-picker", "s")];

/// Scan the prefix table then globals for a binding to `target`'s action.
/// With `exact`, the binding's args must also equal `target.args`.
fn scan(cfg: &KeybindingsCfg, target: &ResolvedAction, exact: bool) -> Option<String> {
    if let Some(chord) = primary_prefix_chord(cfg, target, exact) {
        return Some(format!("{} {chord}", cfg.prefix));
    }
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

fn primary_prefix_chord(
    cfg: &KeybindingsCfg,
    target: &ResolvedAction,
    exact: bool,
) -> Option<&'static str> {
    let chord = PRIMARY_PREFIX_BINDINGS
        .iter()
        .find_map(|(action, chord)| (*action == target.action).then_some(*chord))?;
    let action = cfg.prefix_table.get(chord)?;
    binding_matches(action, target, exact).then_some(chord)
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

    /// The glyphs of a VT stream, CSI escapes dropped.
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

    /// The real palette overflows its modal, so rows past the fold must be
    /// reachable by scrolling (they once painted off the box's bottom edge).
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

    /// [`REGISTRY`] and [`NON_PALETTE_ACTIONS`] partition `ACTION_NAMES`
    /// exactly, so a new `run_action` arm forces a documented home.
    #[test]
    fn every_action_has_exactly_one_doc_home() {
        let dispatched: BTreeSet<&str> = phux_config::vocab::ACTION_NAMES.iter().copied().collect();
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
    fn signal_terminal_palette_default_is_the_reversible_freeze() {
        // ADR-0033: signals are not lease-gated server-side, so the palette
        // default must stay the reversible `freeze`.
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
    fn session_picker_prefers_the_documented_chord_over_its_legacy_alias() {
        let mut cfg =
            phux_config::parse_with_defaults("", std::path::Path::new("<embedded default.toml>"))
                .expect("defaults parse")
                .keybindings;
        let target = ResolvedAction {
            action: "session-picker".to_owned(),
            args: BTreeMap::new(),
        };
        assert_eq!(bound_chord(&cfg, &target).as_deref(), Some("C-a s"));

        cfg.prefix_table.remove("s");
        assert_eq!(
            bound_chord(&cfg, &target).as_deref(),
            Some("C-a a"),
            "the compatibility alias remains discoverable when it is the binding"
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
    fn plugin_row_shows_merged_binding_chord() {
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
}
