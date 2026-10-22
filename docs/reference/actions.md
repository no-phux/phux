# Actions

Every action a key binding, palette row, context menu, or hook can dispatch, with its parameters.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

An action is what a keybinding, palette row, context-menu entry, or hook binds to: a bare name, or a name plus parameters (`{ action = "split-pane", direction = "vertical" }` in config TOML). The table lists every action the TUI dispatcher handles, in the canonical `ACTION_NAMES` order; unit tests pin this inventory to the dispatcher and the command palette, so an action appears here exactly when the binary handles it.

The **Palette** column is the command-palette section the action is offered under; a dash means the palette deliberately has no row for it (reasons follow the table). An empty **Parameters** cell means the action takes none.

| Action | Palette | Parameters | Description |
|---|---|---|---|
| `split-pane` | Pane | `direction` = `horizontal` \| `vertical`; `host?` (satellite); `resource?` (`@N` or `host/@N`) | Split the focused pane side-by-side (vertical divider) |
| `move-pane` | Pane | `target` (local Terminal id; picker-supplied) | Move the focused pane beside another pane… |
| `kill-pane` | Pane |  | Kill the focused pane's terminal |
| `close-view` | Pane |  | Hide the focused pane; its terminal keeps running |
| `reopen-views` | Tab |  | Show hidden terminals, one tab each |
| `new-window` | Tab | `cwd?` (working directory on the attached server's host) | Open a new tab |
| `go-to-directory` | Tab | `path?` (absolute, `~`, or `~/...`; bare starts at the focused pane's directory) | Browse directories on the attached server's host and open a new tab in one |
| `find-path` | Pane | `path?` (directory to browse; defaults to the focused pane's directory) | Browse or fuzzy-search host paths and insert a literal path into the focused pane |
| `insert-path` | Pane | `path` (selected absolute path) | Insert a selected host path without running the command |
| `kill-window` | Tab | `terminals?` (selectors the confirm committed; omitted opens the confirm) | Kill the active tab's terminals… |
| `kill-session` | Session | `id`, `name` (optional pinned session; defaults to current) | Confirm ending all session work, including resources added meanwhile, and delete it |
| `delete-empty-session` | Session | `id`, `name` (optional pinned session; defaults to current) | Confirm deletion of an empty session; never terminates members |
| `organize` | Session |  | Put every pane in the session it belongs in (`phux organize`); moves membership only |
| `organize-session` | Session |  | Move this session's panes to the sessions they belong in; other sessions stay |
| `organize-undo` | Session |  | Reverse the last organize run: panes move back, renames revert, created sessions go |
| `session-actions` | — | `id`, `name`, `host` (pinned picker identity) | Open management for the selected session row |
| `next-window` | Tab |  | Switch to the next tab |
| `previous-window` | Tab |  | Switch to the previous tab |
| `select-window` | — | `index` (0-based tab position) | Focus the tab at a given index |
| `move-window` | — | `index` (0-based destination) or `delta` (signed slots, e.g. `-1`); clamped to the ends | Move the active tab to another position in the tab order |
| `rename-window` | Tab | `name?` (bare opens an interactive prompt) | Rename the active tab (interactive prompt) |
| `rename-session` | Session | `name?` (bare opens an interactive prompt) | Rename the current session (interactive prompt) |
| `focus-direction` | Pane | `direction` = `left` \| `right` \| `up` \| `down` | Move focus to the pane on the left |
| `resize-pane` | Pane | `direction` = `left` \| `right` \| `up` \| `down`; `amount` (cells) | Grow the focused pane to the left |
| `show-help` | — |  | Open the fuzzy commands and help finder |
| `getting-started` | phux |  | Getting started: detach, return, the Agents list, and command discovery |
| `copy-mode` | — |  | Enter copy-mode on the focused pane (scrollback navigation, search, selection, yank) |
| `detach` | phux |  | Detach this client from the session |
| `next-pane` | Pane |  | Cycle focus to the next pane |
| `previous-pane` | Pane |  | Cycle focus to the previous pane |
| `last-pane` | Pane |  | Jump back to the previously focused pane |
| `toggle-zoom` | Pane |  | Zoom the focused pane to fill the tab (toggle) |
| `toggle-sidebar` | phux |  | Show or hide the sidebar (toggle) |
| `command-palette` | — |  | Open the fuzzy commands and help finder |
| `context-menu` | Pane |  | Open the context menu for the focused pane |
| `window-picker` | Tab |  | Pick a tab from all sessions (grouped) |
| `session-picker` | Session |  | Browse sessions and live host availability |
| `agent-fleet` | phux |  | Every agent, its state, and whether it needs you |
| `agents-filter` | phux | `scope` (`all`, `session`, `attention`, or `next`), `host` (`all`, `here`, a satellite name, or `next`); both optional | Narrow the sidebar's Agents panel: every agent, this session's, or the ones that need you, on one machine or all of them. Bare, steps to the next scope |
| `focus-pane` | — | `window` (window index), `pane` (DFS leaf ordinal) | Focus a pane by window index and DFS leaf ordinal |
| `next-attention` | Pane |  | Jump to the next pane waiting for an answer |
| `return-from-attention` | Pane |  | Return to where attention navigation started |
| `switch-session` | — | `name`; `window?` (window index to select after the switch); `pane?` (DFS leaf ordinal to focus in that window); `host?` (a satellite of this hub: opens that session's active pane here through the relay instead of re-attaching) | Re-attach this client to another session |
| `switch-host` | — | `host` (a registered host name, or `local` for this machine); `name` (the session there) | Re-attach this terminal to a session on another machine |
| `toggle-project-group` | — | `project` (the group's key, as the sidebar header carries it) | Collapse or expand one project group in the sidebar's Sessions zone |
| `new-session` | Session | `name?` (bare opens an interactive prompt); `cwd?`; `host?` | Create a new session and switch to it |
| `last-session` | Session |  | Switch back to the previous session |
| `next-session` | Session |  | Switch to the next session by name |
| `previous-session` | Session |  | Switch to the previous session by name |
| `take-input` | Pane |  | Take the wheel: seize exclusive input over the focused pane |
| `give-input` | Pane |  | Give back the wheel: release the focused pane's input lease |
| `signal-terminal` | Pane | `signal` = `interrupt` \| `freeze` \| `resume` \| `terminate` \| `kill` | Signal the focused pane's process group (freeze, resume, kill) |
| `set-pane` | Pane | `mouse` = `on` \| `off` \| `toggle` | Toggle mouse capture for the focused pane |
| `plugin-action` | — | `plugin`, `action` | Run an enabled plugin's manifest action |
| `plugin-pane` | — | `plugin`, `pane` | Open an enabled plugin's manifest pane |
| `reload-config` | phux |  | Reload the config file (keybindings, theme, status bar) |
| `settings` | phux |  | Settings: browse, search, and edit every option in place |
| `report-bug` | phux | `note?` (optional free-text description) | Write a local bug report (session, logs, screen) and copy its path |
| `rec-mark` | phux | `label?` (bare chord writes an empty marker) | Drop an asciicast marker into the recording in progress |

Why the dash rows have no palette entry:

- `command-palette` — it is an entry alias for the finder, so listing it inside the finder would recurse.
- `show-help` — it is an entry alias for the same finder as `command-palette`, so listing it would duplicate that surface.
- `select-window` — parameterized by `index`, which the palette has no UI to collect; the tab picker is the surface for "jump to tab N".
- `move-window` — parameterized by direction; bound to `<` and `>` under the leader, offered in the tab context menu, and done by dragging a tab or a sidebar tab row.
- `session-actions` — opened by Tab in the session picker or right-click on a sidebar session.
- `switch-session` — requires a `name` arg supplied by the session picker (or the fleet's foreign rows), so a bare palette row would have no target to act on.
- `switch-host` — requires `host` and `name` args supplied by the sidebar's machine segments or the session picker's machine groups, so a bare palette row would have no target to act on.
- `toggle-project-group` — requires the `project` arg the sidebar's group header supplies, so a bare palette row would have no group to act on.
- `copy-mode` — a modal input surface entered from its keybinding, not a one-shot command the palette can commit.
- `plugin-action` — its palette rows are built dynamically from enabled plugins' manifests, one per manifest action, carrying `plugin`/`action` args a static row could not supply.
- `plugin-pane` — same shape as `plugin-action`: dynamic rows from enabled plugins' manifest `[[panes]]`, carrying `plugin`/`pane` args.
- `focus-pane` — parameterized by coordinates only the agent-fleet dashboard's rows can supply (the `select-window` precedent).
