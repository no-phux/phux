//! The action registry: one row per action the TUI dispatches, and the
//! single declaration of it (ADR-0172).
//!
//! Each [`ActionSpec`] carries the action's name, its reference-page
//! description and parameters, its [`Palette`] placement (a titled row with
//! the args it commits, or the reason it has none), and the handler
//! `run_action` calls. The palette, the which-key labels, and the generated
//! `docs/reference/actions.md` all render from [`ACTIONS`].
//!
//! The names themselves are config vocabulary and live in phux-config
//! ([`ACTION_NAMES`](phux_config::vocab::ACTION_NAMES)), which validates
//! keybindings and hooks without depending on this crate; a test pins
//! [`ACTIONS`] to that list exactly. Bound chords resolve from the live
//! [`KeybindingsCfg`] at build time.

use std::collections::BTreeMap;

use phux_config::keybind::ResolvedAction;
use phux_config::{Action, KeybindingsCfg};

use super::input_dispatch::run_action::ActionHandler;
use super::plugin_actions::PluginActionEntry;
use super::plugin_panes::PluginPaneEntry;
use crate::render::overlay::select_list::SelectItem;

/// The section a palette action groups under, in palette order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Pane-level actions: split, kill, focus, resize, zoom, cycle.
    Pane,
    /// Tab actions: new/kill/cycle/rename/pick.
    Window,
    /// Session actions: new/rename/pick.
    Session,
    /// phux's own chrome: sidebar, help, detach.
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
            Self::Window => "Tab",
            Self::Session => "Session",
            Self::View => "phux",
        }
    }
}

/// A registry row: one dispatched action, declared once.
#[derive(Debug, Clone, Copy)]
pub struct ActionSpec {
    /// Canonical action name (an
    /// [`ACTION_NAMES`](phux_config::vocab::ACTION_NAMES) entry).
    pub name: &'static str,
    /// One-line human description for the generated actions reference.
    pub description: &'static str,
    /// The parameter surface for the generated reference page (`""` for a
    /// bare action); documentation only.
    pub params: &'static str,
    /// Where the command palette offers it, if anywhere.
    pub palette: Palette,
    /// What dispatching it does.
    pub(in crate::attach) run: ActionHandler,
}

/// An action's place in the command palette.
#[derive(Debug, Clone, Copy)]
pub enum Palette {
    /// A row under `category`.
    Row {
        /// Short title the palette lists, in the sentence case of a menu
        /// item (`Split right`); a trailing ellipsis marks one that asks
        /// for more.
        title: &'static str,
        /// The section the row groups under.
        category: Category,
        /// Inline args the row commits, so it commits a runnable action
        /// rather than a half-specified one that would bell.
        args: &'static [(&'static str, ArgValue)],
    },
    /// Deliberately no row, for `reason` (rendered in the generated
    /// reference so the omission reads as deliberate).
    Hidden {
        /// Why the palette has no row for it.
        reason: &'static str,
    },
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
    /// The [`ResolvedAction`] this action's palette row commits, the same
    /// shape a keybinding produces (no args for a hidden action).
    #[must_use]
    pub fn resolved_action(&self) -> ResolvedAction {
        let args = match self.palette {
            Palette::Row { args, .. } => args,
            Palette::Hidden { .. } => &[],
        };
        ResolvedAction {
            action: self.name.to_owned(),
            args: args
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.to_toml()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    /// The palette title and section, when the palette offers this action.
    #[must_use]
    pub const fn palette_row(&self) -> Option<(&'static str, Category)> {
        match self.palette {
            Palette::Row {
                title, category, ..
            } => Some((title, category)),
            Palette::Hidden { .. } => None,
        }
    }
}

/// An extra palette row for an action whose one registry row lists only
/// one of its argument values: the same action under another title, so the
/// palette offers what which-key already shows (`"` splits below, `%`
/// splits right).
struct PaletteVariant {
    /// The [`ActionSpec::name`] the row commits.
    action: &'static str,
    /// The row's title, in the registry row's sentence case.
    title: &'static str,
    /// The args that make it this variant.
    args: &'static [(&'static str, ArgValue)],
}

/// The variant rows, each listed right after its action's own row.
const PALETTE_VARIANTS: &[PaletteVariant] = &[
    PaletteVariant {
        action: "split-pane",
        title: "Split below",
        args: &[("direction", ArgValue::Str("horizontal"))],
    },
    PaletteVariant {
        action: "focus-direction",
        title: "Focus pane on the right",
        args: &[("direction", ArgValue::Str("right"))],
    },
    PaletteVariant {
        action: "focus-direction",
        title: "Focus pane above",
        args: &[("direction", ArgValue::Str("up"))],
    },
    PaletteVariant {
        action: "focus-direction",
        title: "Focus pane below",
        args: &[("direction", ArgValue::Str("down"))],
    },
];

/// The registry row named `name`, if any.
#[must_use]
pub fn find(name: &str) -> Option<&'static ActionSpec> {
    ACTIONS.iter().find(|spec| spec.name == name)
}

mod hidden_rows;
mod palette_rows;

use hidden_rows::HIDDEN_ROWS;
use palette_rows::PALETTE_ROWS;

/// Every dispatched action: the palette's rows in palette order, then the
/// actions it deliberately omits. A test pins the names to `ACTION_NAMES`.
pub const ACTIONS: &[ActionSpec] = &concat_rows();

/// [`PALETTE_ROWS`] then [`HIDDEN_ROWS`], joined at compile time.
const fn concat_rows() -> [ActionSpec; PALETTE_ROWS.len() + HIDDEN_ROWS.len()] {
    let mut rows = [PALETTE_ROWS[0]; PALETTE_ROWS.len() + HIDDEN_ROWS.len()];
    let mut i = 0;
    while i < PALETTE_ROWS.len() {
        rows[i] = PALETTE_ROWS[i];
        i += 1;
    }
    let mut j = 0;
    while j < HIDDEN_ROWS.len() {
        rows[PALETTE_ROWS.len() + j] = HIDDEN_ROWS[j];
        j += 1;
    }
    rows
}

/// The palette rows.
///
/// The [`ACTIONS`] with a [`Palette::Row`] grouped under [`Category`] headers, each row annotated by its
/// bound chord (nothing without one or without a config: a menu is not a
/// table), then enabled plugins' actions and panes under a trailing
/// **Plugin** header.
#[must_use]
pub fn palette_items(
    keybindings: Option<&KeybindingsCfg>,
    plugin_actions: &[PluginActionEntry],
    plugin_panes: &[PluginPaneEntry],
) -> Vec<SelectItem> {
    let mut items = Vec::new();
    for &category in Category::ORDER {
        let mut header_pushed = false;
        let rows = ACTIONS
            .iter()
            .filter_map(|spec| Some((spec, spec.palette_row()?)))
            .filter(|(_, (_, section))| *section == category);
        for (spec, (title, _)) in rows {
            if !header_pushed {
                items.push(SelectItem::header(category.header()));
                header_pushed = true;
            }
            let resolved = spec.resolved_action();
            items.push(
                chord_annotated(SelectItem::new(title, resolved), keybindings)
                    .keywords(spec.name)
                    .indented(),
            );
            for variant in PALETTE_VARIANTS
                .iter()
                .filter(|variant| variant.action == spec.name)
            {
                let resolved = ResolvedAction {
                    action: spec.name.to_owned(),
                    args: variant
                        .args
                        .iter()
                        .map(|(k, v)| ((*k).to_owned(), v.to_toml()))
                        .collect::<BTreeMap<_, _>>(),
                };
                items.push(
                    chord_annotated(SelectItem::new(variant.title, resolved), keybindings)
                        .keywords(spec.name)
                        .indented(),
                );
            }
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
        items.push(chord_annotated(SelectItem::new(label, resolved), keybindings).indented());
    }
    items
}

/// `item` with its bound chord's literal keystrokes as the secondary, when
/// one is bound.
fn chord_annotated(item: SelectItem, keybindings: Option<&KeybindingsCfg>) -> SelectItem {
    match bound_chord_for(keybindings, &item.action) {
        Some(chord) => item.secondary(chord),
        None => item,
    }
}

/// The chord bound to `resolved`, or `None`, which the palette and context
/// menus both leave blank. Both use this one resolver so they never disagree.
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
mod tests;
