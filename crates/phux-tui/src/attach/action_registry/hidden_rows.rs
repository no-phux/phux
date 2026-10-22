//! The registry rows the command palette deliberately omits, each with the
//! reason the generated reference prints.

use super::{ActionSpec, Palette};
use crate::attach::input_dispatch::run_action as run;
use crate::attach::input_dispatch::session_management;

/// The actions with no palette row.
pub(super) const HIDDEN_ROWS: &[ActionSpec] = &[
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
                      Sessions zone",
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
        params: "`plugin` (the plugin's id), `id` (the manifest action's id)",
        palette: Palette::Hidden {
            reason: "its palette rows are built dynamically from enabled \
                     plugins' manifests, one per manifest action, carrying \
                     `plugin`/`id` args a static row could not supply",
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
    ActionSpec {
        name: "enter-mode",
        description: "Turn on a key mode: its keys work without the prefix until \
                      exit-mode or Esc",
        params: "`name` (a `[keybindings.modes.<name>]` table); `once?` (`true` \
                 leaves after one of the mode's keys)",
        palette: Palette::Hidden {
            reason: "parameterized by `name`, a mode the config defines; reached \
                     from its binding (the shipped resize mode is the prefix then `R`)",
        },
        run: run::enter_mode,
    },
    ActionSpec {
        name: "exit-mode",
        description: "Leave the active key mode",
        params: "",
        palette: Palette::Hidden {
            reason: "only meaningful inside a key mode, where its own key (or Esc) \
                     reaches it",
        },
        run: run::exit_mode,
    },
];
