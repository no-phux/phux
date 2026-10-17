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

use super::input_dispatch::run_action::{self as run, ActionHandler};
use super::input_dispatch::{organize, session_management};
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

/// Every dispatched action: the palette's rows in palette order, then the
/// actions it deliberately omits. A test pins the names to `ACTION_NAMES`.
pub const ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        name: "split-pane",
        description: "Split the focused pane side-by-side (vertical divider)",
        params: "`direction` = `horizontal` | `vertical`; `host?` (satellite); `resource?` (`@N` or `host/@N`)",
        palette: Palette::Row {
            title: "Split right",
            category: Category::Pane,
            args: &[("direction", ArgValue::Str("vertical"))],
        },
        run: run::split_pane,
    },
    ActionSpec {
        name: "move-pane",
        description: "Move the focused pane beside another pane…",
        params: "`target` (local Terminal id; picker-supplied)",
        palette: Palette::Row {
            title: "Move pane…",
            category: Category::Pane,
            args: &[],
        },
        run: run::move_pane,
    },
    ActionSpec {
        name: "kill-pane",
        description: "Kill the focused pane's terminal",
        params: "",
        palette: Palette::Row {
            title: "Kill pane",
            category: Category::Pane,
            args: &[],
        },
        run: run::kill_focused_pane,
    },
    ActionSpec {
        name: "close-view",
        description: "Hide the focused pane; its terminal keeps running",
        params: "",
        palette: Palette::Row {
            title: "Hide pane (keeps running)",
            category: Category::Pane,
            args: &[],
        },
        run: run::close_view,
    },
    ActionSpec {
        name: "reopen-views",
        description: "Show hidden terminals, one tab each",
        params: "",
        palette: Palette::Row {
            title: "Reopen hidden terminals",
            category: Category::Window,
            args: &[],
        },
        run: run::reopen_views,
    },
    ActionSpec {
        name: "focus-direction",
        description: "Move focus to the pane on the left",
        params: "`direction` = `left` | `right` | `up` | `down`",
        palette: Palette::Row {
            title: "Focus pane on the left",
            category: Category::Pane,
            args: &[("direction", ArgValue::Str("left"))],
        },
        run: run::focus_direction,
    },
    ActionSpec {
        name: "resize-pane",
        description: "Grow the focused pane to the left",
        params: "`direction` = `left` | `right` | `up` | `down`; `amount` (cells)",
        palette: Palette::Row {
            title: "Grow pane left",
            category: Category::Pane,
            args: &[
                ("direction", ArgValue::Str("left")),
                ("amount", ArgValue::Int(5)),
            ],
        },
        run: run::resize_pane,
    },
    ActionSpec {
        name: "next-pane",
        description: "Cycle focus to the next pane",
        params: "",
        palette: Palette::Row {
            title: "Next pane",
            category: Category::Pane,
            args: &[],
        },
        run: run::next_pane,
    },
    ActionSpec {
        name: "next-attention",
        description: "Jump to the next pane waiting for an answer",
        params: "",
        palette: Palette::Row {
            title: "Next waiting pane",
            category: Category::Pane,
            args: &[],
        },
        run: run::next_attention,
    },
    ActionSpec {
        name: "return-from-attention",
        description: "Return to where attention navigation started",
        params: "",
        palette: Palette::Row {
            title: "Back where you were",
            category: Category::Pane,
            args: &[],
        },
        run: run::return_from_attention,
    },
    ActionSpec {
        name: "previous-pane",
        description: "Cycle focus to the previous pane",
        params: "",
        palette: Palette::Row {
            title: "Previous pane",
            category: Category::Pane,
            args: &[],
        },
        run: run::previous_pane,
    },
    ActionSpec {
        name: "last-pane",
        description: "Jump back to the previously focused pane",
        params: "",
        palette: Palette::Row {
            title: "Last pane",
            category: Category::Pane,
            args: &[],
        },
        run: run::last_pane,
    },
    ActionSpec {
        name: "toggle-zoom",
        description: "Zoom the focused pane to fill the tab (toggle)",
        params: "",
        palette: Palette::Row {
            title: "Zoom pane",
            category: Category::Pane,
            args: &[],
        },
        run: run::toggle_zoom,
    },
    ActionSpec {
        name: "context-menu",
        description: "Open the context menu for the focused pane",
        params: "",
        palette: Palette::Row {
            title: "Pane menu",
            category: Category::Pane,
            args: &[],
        },
        run: run::push_context_menu,
    },
    ActionSpec {
        name: "new-window",
        description: "Open a new tab",
        params: "`cwd?` (working directory on the attached server's host)",
        palette: Palette::Row {
            title: "New tab",
            category: Category::Window,
            args: &[],
        },
        run: run::new_window,
    },
    ActionSpec {
        name: "go-to-directory",
        description: "Browse directories on the attached server's host and open a new tab in one",
        params: "`path?` (absolute, `~`, or `~/...`; bare starts at the focused pane's directory)",
        palette: Palette::Row {
            title: "New tab in folder…",
            category: Category::Window,
            args: &[],
        },
        run: run::go_to_directory,
    },
    ActionSpec {
        name: "find-path",
        description: "Browse or fuzzy-search host paths and insert a literal path into the focused pane",
        params: "`path?` (directory to browse; defaults to the focused pane's directory)",
        palette: Palette::Row {
            title: "Insert path…",
            category: Category::Pane,
            args: &[],
        },
        run: run::find_path,
    },
    ActionSpec {
        name: "insert-path",
        description: "Insert a selected host path without running the command",
        params: "`path` (selected absolute path)",
        palette: Palette::Row {
            title: "Insert selected path",
            category: Category::Pane,
            args: &[],
        },
        run: run::insert_path,
    },
    ActionSpec {
        name: "kill-window",
        description: "Kill the active tab's terminals…",
        params: "`terminals?` (selectors the confirm committed; omitted opens the confirm)",
        palette: Palette::Row {
            title: "Kill tab…",
            category: Category::Window,
            args: &[],
        },
        run: run::kill_window,
    },
    ActionSpec {
        name: "next-window",
        description: "Switch to the next tab",
        params: "",
        palette: Palette::Row {
            title: "Next tab",
            category: Category::Window,
            args: &[],
        },
        run: run::next_window,
    },
    ActionSpec {
        name: "previous-window",
        description: "Switch to the previous tab",
        params: "",
        palette: Palette::Row {
            title: "Previous tab",
            category: Category::Window,
            args: &[],
        },
        run: run::previous_window,
    },
    ActionSpec {
        name: "window-picker",
        description: "Pick a tab from all sessions (grouped)",
        params: "",
        palette: Palette::Row {
            title: "Go to tab…",
            category: Category::Window,
            args: &[],
        },
        run: run::push_window_picker,
    },
    ActionSpec {
        name: "rename-window",
        description: "Rename the active tab (interactive prompt)",
        params: "`name?` (bare opens an interactive prompt)",
        palette: Palette::Row {
            title: "Rename tab…",
            category: Category::Window,
            args: &[],
        },
        run: run::rename_window,
    },
    ActionSpec {
        name: "session-picker",
        description: "Browse sessions and live host availability",
        params: "",
        palette: Palette::Row {
            title: "Go to session…",
            category: Category::Session,
            args: &[],
        },
        run: run::push_session_picker,
    },
    ActionSpec {
        name: "kill-session",
        description: "Confirm ending all session work, including resources added meanwhile, and delete it",
        params: "`id`, `name` (optional pinned session; defaults to current)",
        palette: Palette::Row {
            title: "Kill session…",
            category: Category::Session,
            args: &[],
        },
        run: session_management::queue,
    },
    ActionSpec {
        name: "delete-empty-session",
        description: "Confirm deletion of an empty session; never terminates members",
        params: "`id`, `name` (optional pinned session; defaults to current)",
        palette: Palette::Row {
            title: "Delete empty session…",
            category: Category::Session,
            args: &[],
        },
        run: session_management::queue,
    },
    ActionSpec {
        name: "organize",
        description: "Put every pane in the session it belongs in (`phux organize`); moves membership only",
        params: "",
        palette: Palette::Row {
            title: "Organize everything",
            category: Category::Session,
            args: &[],
        },
        run: organize::organize,
    },
    ActionSpec {
        name: "organize-session",
        description: "Move this session's panes to the sessions they belong in; other sessions stay",
        params: "",
        palette: Palette::Row {
            title: "Organize this session",
            category: Category::Session,
            args: &[],
        },
        run: organize::organize,
    },
    ActionSpec {
        name: "organize-undo",
        description: "Reverse the last organize run: panes move back, renames revert, created sessions go",
        params: "",
        palette: Palette::Row {
            title: "Undo organize",
            category: Category::Session,
            args: &[],
        },
        run: organize::organize,
    },
    ActionSpec {
        name: "new-session",
        description: "Create a new session and switch to it",
        params: "`name?` (bare opens an interactive prompt); `cwd?`; `host?`",
        palette: Palette::Row {
            title: "New session…",
            category: Category::Session,
            args: &[],
        },
        run: run::new_session,
    },
    ActionSpec {
        name: "last-session",
        description: "Switch back to the previous session",
        params: "",
        palette: Palette::Row {
            title: "Last session",
            category: Category::Session,
            args: &[],
        },
        run: run::last_session,
    },
    ActionSpec {
        name: "next-session",
        description: "Switch to the next session by name",
        params: "",
        palette: Palette::Row {
            title: "Next session",
            category: Category::Session,
            args: &[],
        },
        run: run::next_session,
    },
    ActionSpec {
        name: "previous-session",
        description: "Switch to the previous session by name",
        params: "",
        palette: Palette::Row {
            title: "Previous session",
            category: Category::Session,
            args: &[],
        },
        run: run::previous_session,
    },
    ActionSpec {
        name: "rename-session",
        description: "Rename the current session (interactive prompt)",
        params: "`name?` (bare opens an interactive prompt)",
        palette: Palette::Row {
            title: "Rename session…",
            category: Category::Session,
            args: &[],
        },
        run: run::rename_session,
    },
    ActionSpec {
        name: "toggle-sidebar",
        description: "Show or hide the sidebar (toggle)",
        params: "",
        palette: Palette::Row {
            title: "Toggle sidebar",
            category: Category::View,
            args: &[],
        },
        run: run::toggle_sidebar,
    },
    ActionSpec {
        name: "agent-fleet",
        description: "Every agent, its state, and whether it needs you",
        params: "",
        palette: Palette::Row {
            title: "Agents",
            category: Category::View,
            args: &[],
        },
        run: run::push_agent_fleet,
    },
    ActionSpec {
        name: "settings",
        description: "Settings: browse, search, and edit every option in place",
        params: "",
        palette: Palette::Row {
            title: "Settings",
            category: Category::View,
            args: &[],
        },
        run: run::push_settings,
    },
    ActionSpec {
        name: "report-bug",
        description: "Write a local bug report (session, logs, screen) and copy its path",
        params: "`note?` (optional free-text description)",
        palette: Palette::Row {
            title: "Report a bug",
            category: Category::View,
            args: &[],
        },
        run: run::report_bug,
    },
    ActionSpec {
        name: "rec-mark",
        description: "Drop an asciicast marker into the recording in progress",
        params: "`label?` (bare chord writes an empty marker)",
        palette: Palette::Row {
            title: "Mark recording",
            category: Category::View,
            args: &[],
        },
        run: run::rec_mark,
    },
    ActionSpec {
        name: "getting-started",
        description: "Getting started: detach, return, the Agents list, and command discovery",
        params: "",
        palette: Palette::Row {
            title: "Getting started",
            category: Category::View,
            args: &[],
        },
        run: run::push_getting_started,
    },
    ActionSpec {
        name: "detach",
        description: "Detach this client from the session",
        params: "",
        palette: Palette::Row {
            title: "Detach",
            category: Category::View,
            args: &[],
        },
        run: run::detach,
    },
    ActionSpec {
        name: "reload-config",
        description: "Reload the config file (keybindings, theme, status bar)",
        params: "",
        palette: Palette::Row {
            title: "Reload config",
            category: Category::View,
            args: &[],
        },
        run: run::reload_config,
    },
    ActionSpec {
        name: "take-input",
        description: "Take the wheel: seize exclusive input over the focused pane",
        params: "",
        palette: Palette::Row {
            title: "Take the wheel",
            category: Category::Pane,
            args: &[],
        },
        run: run::take_input,
    },
    ActionSpec {
        name: "give-input",
        description: "Give back the wheel: release the focused pane's input lease",
        params: "",
        palette: Palette::Row {
            title: "Give back the wheel",
            category: Category::Pane,
            args: &[],
        },
        run: run::give_input,
    },
    ActionSpec {
        name: "signal-terminal",
        description: "Signal the focused pane's process group (freeze, resume, kill)",
        params: "`signal` = `interrupt` | `freeze` | `resume` | `terminate` | `kill`",
        palette: Palette::Row {
            title: "Freeze pane",
            category: Category::Pane,
            args: &[("signal", ArgValue::Str("freeze"))],
        },
        run: run::signal_terminal,
    },
    ActionSpec {
        name: "set-pane",
        description: "Toggle mouse capture for the focused pane",
        params: "`mouse` = `on` | `off` | `toggle`",
        palette: Palette::Row {
            title: "Toggle mouse capture",
            category: Category::Pane,
            args: &[("mouse", ArgValue::Str("toggle"))],
        },
        run: run::set_pane,
    },
    ActionSpec {
        name: "command-palette",
        description: "Open the fuzzy commands and help finder",
        params: "",
        palette: Palette::Hidden {
            reason: "it is an entry alias for the finder, so listing it inside the finder would recurse",
        },
        run: run::push_action_finder,
    },
    ActionSpec {
        name: "show-help",
        description: "Open the fuzzy commands and help finder",
        params: "",
        palette: Palette::Hidden {
            reason: "it is an entry alias for the same finder as `command-palette`, so listing it would duplicate that surface",
        },
        run: run::push_action_finder,
    },
    ActionSpec {
        name: "select-window",
        description: "Focus the tab at a given index",
        params: "`index` (0-based tab position)",
        palette: Palette::Hidden {
            reason: "parameterized by `index`, which the palette has no UI to \
                     collect; the tab picker is the surface for \"jump to \
                     tab N\"",
        },
        run: run::select_window,
    },
    ActionSpec {
        name: "move-window",
        description: "Move the active tab to another position in the tab order",
        params: "`index` (0-based destination) or `delta` (signed slots, e.g. `-1`); \
                 clamped to the ends",
        palette: Palette::Hidden {
            reason: "parameterized by direction; bound to `<` and `>` under the leader, \
                     offered in the tab context menu, and done by dragging a tab \
                     or a sidebar tab row",
        },
        run: run::move_window,
    },
    ActionSpec {
        name: "session-actions",
        description: "Open management for the selected session row",
        params: "`id`, `name`, `host` (pinned picker identity)",
        palette: Palette::Hidden {
            reason: "opened by Tab in the session picker or right-click on a sidebar session",
        },
        run: session_management::push_actions,
    },
    ActionSpec {
        name: "switch-session",
        description: "Re-attach this client to another session",
        params: "`name`; `window?` (window index to select after the \
                 switch); `pane?` (DFS leaf ordinal to focus in that \
                 window); `host?` (a satellite of this hub: opens that \
                 session's active pane here through the relay instead of \
                 re-attaching)",
        palette: Palette::Hidden {
            reason: "requires a `name` arg supplied by the session picker (or \
                     the fleet's foreign rows), so a bare palette row would \
                     have no target to act on",
        },
        run: run::switch_session,
    },
    ActionSpec {
        name: "switch-host",
        description: "Re-attach this terminal to a session on another machine",
        params: "`host` (a registered host name, or `local` for this \
                 machine); `name` (the session there)",
        palette: Palette::Hidden {
            reason: "requires `host` and `name` args supplied by the sidebar's \
                     machine segments or the session picker's machine groups, \
                     so a bare palette row would have no target to act on",
        },
        run: run::switch_host,
    },
    ActionSpec {
        name: "toggle-project-group",
        description: "Collapse or expand one project group in the sidebar's \
                      Sessions zone (ADR-0168)",
        params: "`project` (the group's key, as the sidebar header carries it)",
        palette: Palette::Hidden {
            reason: "requires the `project` arg the sidebar's group header supplies, \
                     so a bare palette row would have no group to act on",
        },
        run: run::toggle_project_group,
    },
    ActionSpec {
        name: "copy-mode",
        description: "Enter copy-mode on the focused pane (scrollback \
                      navigation, search, selection, yank)",
        params: "",
        palette: Palette::Hidden {
            reason: "a modal input surface entered from its keybinding, not a \
                     one-shot command the palette can commit",
        },
        run: run::push_copy_mode,
    },
    ActionSpec {
        name: "plugin-action",
        description: "Run an enabled plugin's manifest action",
        params: "`plugin`, `action`",
        palette: Palette::Hidden {
            reason: "its palette rows are built dynamically from enabled \
                     plugins' manifests, one per manifest action, carrying \
                     `plugin`/`action` args a static row could not supply",
        },
        run: run::plugin_action,
    },
    ActionSpec {
        name: "plugin-pane",
        description: "Open an enabled plugin's manifest pane",
        params: "`plugin`, `pane`",
        palette: Palette::Hidden {
            reason: "same shape as `plugin-action`: dynamic rows from enabled \
                     plugins' manifest `[[panes]]`, carrying `plugin`/`pane` \
                     args",
        },
        run: run::plugin_pane,
    },
    ActionSpec {
        name: "focus-pane",
        description: "Focus a pane by window index and DFS leaf ordinal",
        params: "`window` (window index), `pane` (DFS leaf ordinal)",
        palette: Palette::Hidden {
            reason: "parameterized by coordinates only the agent-fleet \
                     dashboard's rows can supply (the `select-window` \
                     precedent)",
        },
        run: run::focus_pane,
    },
];

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
            opened.contains('┃'),
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

    /// [`ACTIONS`] declares every `ACTION_NAMES` entry exactly once and
    /// nothing else, so config validation and dispatch agree.
    #[test]
    fn the_registry_declares_every_action_name_exactly_once() {
        let names: BTreeSet<&str> = phux_config::vocab::ACTION_NAMES.iter().copied().collect();
        assert_eq!(
            names.len(),
            phux_config::vocab::ACTION_NAMES.len(),
            "ACTION_NAMES repeats a name",
        );
        let mut declared = BTreeSet::new();
        for spec in ACTIONS {
            assert!(
                declared.insert(spec.name),
                "`{}` is declared twice",
                spec.name
            );
            assert!(
                names.contains(spec.name),
                "`{}` is declared but missing from phux_config::vocab::ACTION_NAMES, \
                 so config validation would reject a binding to it",
                spec.name,
            );
        }
        for name in &names {
            assert!(
                declared.contains(name),
                "`{name}` is in ACTION_NAMES but has no ActionSpec in ACTIONS",
            );
        }
    }

    /// Every row documents itself, and a hidden one says why.
    #[test]
    fn every_action_is_documented() {
        for spec in ACTIONS {
            assert!(
                !spec.description.trim().is_empty(),
                "`{}` has an empty description",
                spec.name
            );
            if let Palette::Hidden { reason } = spec.palette {
                assert!(
                    !reason.trim().is_empty(),
                    "`{}` has an empty reason",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn signal_terminal_palette_default_is_the_reversible_freeze() {
        // ADR-0033: signals are not lease-gated server-side, so the palette
        // default must stay the reversible `freeze`.
        let sig = find("signal-terminal").expect("signal-terminal registered");
        assert_eq!(
            sig.resolved_action().args.get("signal"),
            Some(&toml::Value::String("freeze".to_owned())),
            "the palette default signal must remain the reversible freeze",
        );
    }

    /// The palette offers both split directions, each committing the
    /// `direction` its title says, so `spl` finds "Split below" as well as
    /// "Split right".
    #[test]
    fn palette_lists_split_below_beside_split_right() {
        let items = palette_items(None, &[], &[]);
        let direction_of = |title: &str| {
            items
                .iter()
                .find(|i| i.label == title)
                .unwrap_or_else(|| panic!("{title} row"))
                .action
                .args
                .get("direction")
                .and_then(toml::Value::as_str)
                .map(ToOwned::to_owned)
        };
        assert_eq!(direction_of("Split right").as_deref(), Some("vertical"));
        assert_eq!(direction_of("Split below").as_deref(), Some("horizontal"));
        let pane_rows: Vec<&str> = items
            .iter()
            .take_while(|i| i.label != "Tab")
            .filter(|i| !i.is_header())
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(&pane_rows[..2], ["Split right", "Split below"]);
    }

    #[test]
    fn palette_items_show_no_chord_when_no_config() {
        let items = palette_items(None, &[], &[]);
        assert!(
            items
                .iter()
                .filter(|i| !i.is_header())
                .all(|i| i.secondary.is_none()),
            "no config ⇒ no row claims a chord",
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
        assert_eq!(headers, vec!["Pane", "Tab", "Session", "phux"]);

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
        assert_eq!(headers, vec!["Pane", "Tab", "Session", "phux", "Plugin"]);

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
        assert_eq!(headers, vec!["Pane", "Tab", "Session", "phux", "Plugin"]);

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
