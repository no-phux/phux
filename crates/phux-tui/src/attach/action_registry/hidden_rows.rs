//! The registry rows the command palette deliberately omits, each with the
//! reason the generated reference prints.

use super::{ActionSpec, Palette};
use crate::attach::input_dispatch::run_action as run;
use crate::attach::input_dispatch::session_management;

/// The actions with no palette row.
pub(super) const HIDDEN_ROWS: &[ActionSpec] = &[
    hidden! { "copy-mode-key", run::copy_mode_key,
    "it only means something as an entry of `[keybindings.modes.copy-mode]`",
    "In the `copy-mode` key mode, make a key stand for a built-in copy-mode key",
    "`key` (the built-in key, e.g. `j` or `C-b`)" },
    hidden! { "copy-text", run::copy_text,
    "it is what a row of the link picker does; on its own it has no text to copy",
    "Copy the given text to the clipboard",
    "`text`" },
    hidden! { "command-palette", run::push_action_finder,
    "it is an entry alias for the finder, so listing it inside the finder would recurse",
    "Open the fuzzy commands and help finder",
    "" },
    hidden! { "show-help", run::push_action_finder,
    "it is an entry alias for the same finder as `command-palette`, so listing it would duplicate that surface",
    "Open the fuzzy commands and help finder",
    "" },
    hidden! { "select-window", run::select_window,
    "parameterized by `index`, which the palette has no UI to \
                 collect; the tab picker is the surface for \"jump to \
                 tab N\"",
    "Focus the tab at a given index",
    "`index` (0-based tab position)" },
    hidden! { "move-window", run::move_window,
    "parameterized by direction; bound to `<` and `>` under the leader, \
                 offered in the tab context menu, and done by dragging a tab \
                 or a sidebar tab row",
    "Move the active tab to another position in the tab order",
    "`index` (0-based destination) or `delta` (signed slots, e.g. `-1`); \
                 clamped to the ends" },
    hidden! { "session-actions", session_management::push_actions,
    "opened by Tab in the session picker or right-click on a sidebar session",
    "Open management for the selected session row",
    "`id`, `name`, `host` (pinned picker identity)" },
    hidden! { "switch-session", run::switch_session,
    "requires a `name` arg supplied by the session picker (or \
                 the fleet's foreign rows), so a bare palette row would \
                 have no target to act on",
    "Re-attach this client to another session",
    "`name`; `window?` (window index to select after the \
                 switch); `pane?` (DFS leaf ordinal to focus in that \
                 window); `host?` (a satellite of this hub: opens that \
                 session's active pane here through the relay instead of \
                 re-attaching)" },
    hidden! { "switch-host", run::switch_host,
    "requires `host` and `name` args supplied by the sidebar's \
                 machine segments or the session picker's machine groups, \
                 so a bare palette row would have no target to act on",
    "Re-attach this terminal to a session on another machine",
    "`host` (a registered host name, or `local` for this \
                 machine); `name` (the session there)" },
    hidden! { "toggle-project-group", run::toggle_project_group,
    "requires the `project` arg the sidebar's group header supplies, \
                 so a bare palette row would have no group to act on",
    "Collapse or expand one project group in the sidebar's \
                      Sessions zone",
    "`project` (the group's key, as the sidebar header carries it)" },
    hidden! { "copy-mode", run::push_copy_mode,
    "a modal input surface entered from its keybinding, not a \
                 one-shot command the palette can commit",
    "Enter copy-mode on the focused pane (scrollback \
                      navigation, search, selection, yank)",
    "" },
    hidden! { "plugin-action", run::plugin_action,
    "its palette rows are built dynamically from enabled \
                 plugins' manifests, one per manifest action, carrying \
                 `plugin`/`id` args a static row could not supply",
    "Run an enabled plugin's manifest action",
    "`plugin` (the plugin's id), `id` (the manifest action's id)" },
    hidden! { "plugin-pane", run::plugin_pane,
    "same shape as `plugin-action`: dynamic rows from enabled \
                 plugins' manifest `[[panes]]`, carrying `plugin`/`pane` \
                 args",
    "Open an enabled plugin's manifest pane",
    "`plugin`, `pane`" },
    hidden! { "focus-pane", run::focus_pane,
    "parameterized by coordinates only the agent-fleet \
                 dashboard's rows can supply (the `select-window` \
                 precedent)",
    "Focus a pane by window index and DFS leaf ordinal",
    "`window` (window index), `pane` (DFS leaf ordinal)" },
    hidden! { "enter-mode", run::enter_mode,
    "parameterized by `name`, a mode the config defines; reached \
                 from its binding (the shipped resize mode is the prefix then `R`)",
    "Turn on a key mode: its keys work without the prefix until \
                      exit-mode or Esc",
    "`name` (a `[keybindings.modes.<name>]` table); `once?` (`true` \
                 leaves after one of the mode's keys)" },
    hidden! { "exit-mode", run::exit_mode,
    "only meaningful inside a key mode, where its own key (or Esc) \
                 reaches it",
    "Leave the active key mode",
    "" },
];
