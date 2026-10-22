//! The registry rows the command palette offers, in palette order.

use super::{ActionSpec, ArgValue, Category, Palette};
use crate::attach::input_dispatch::run_action as run;
use crate::attach::input_dispatch::{organize, session_management};

/// The actions with a palette row, in the order the palette lists them.
pub(super) const PALETTE_ROWS: &[ActionSpec] = &[
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
        name: "agents-filter",
        description: "Narrow the sidebar's Agents panel: every agent, this \
                      session's, or the ones that need you, on one machine \
                      or all of them. Bare, steps to the next scope",
        params: "`scope` (`all`, `session`, `attention`, or `next`), `host` \
                 (`all`, `here`, a satellite name, or `next`); both optional",
        palette: Palette::Row {
            title: "Cycle agents filter",
            category: Category::View,
            args: &[],
        },
        run: run::agents_filter,
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
];
