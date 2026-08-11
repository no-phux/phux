---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-08-02
---

# phux actions reference

**TL;DR.** Every action a keybinding, palette row, context menu, or hook can dispatch, with its parameter surface and where the command palette offers it. Rendered from the same in-code inventories the dispatcher and palette are test-pinned to, so an action is listed here exactly when the binary handles it.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

An action is what a keybinding, palette row, context-menu entry, or hook binds to: a bare name, or a name plus parameters (`{ action = "split-pane", direction = "vertical" }` in config TOML). The table lists every action the TUI dispatcher handles, in the canonical `ACTION_NAMES` order; unit tests pin this inventory to the dispatcher and the command palette, so an action appears here exactly when the binary handles it.

The **Palette** column is the command-palette section the action is offered under; a dash means the palette deliberately has no row for it (reasons follow the table). An empty **Parameters** cell means the action takes none.

| Action | Palette | Parameters | Description |
|---|---|---|---|
| `split-pane` | Pane | `direction` = `horizontal` \| `vertical` | Split the focused pane side-by-side (vertical divider) |
| `kill-pane` | Pane |  | Close the focused pane |
| `new-window` | Window |  | Open a new window |
| `kill-window` | Window |  | Close the active window and all its panes |
| `next-window` | Window |  | Switch to the next window |
| `previous-window` | Window |  | Switch to the previous window |
| `select-window` | — | `index` (0-based window position) | Focus the window at a given index |
| `rename-window` | Window | `name?` (bare opens an interactive prompt) | Rename the active window (interactive prompt) |
| `rename-session` | Session | `name?` (bare opens an interactive prompt) | Rename the current session (interactive prompt) |
| `focus-direction` | Pane | `direction` = `left` \| `right` \| `up` \| `down` | Move focus to the pane on the left |
| `resize-pane` | Pane | `direction` = `left` \| `right` \| `up` \| `down`; `amount` (cells) | Grow the focused pane to the left |
| `show-help` | View |  | Show the keybindings help overlay |
| `getting-started` | View |  | Getting started: detach, return, and command discovery |
| `copy-mode` | — |  | Enter copy-mode on the focused pane (scrollback navigation, selection, yank) |
| `detach` | View |  | Detach this client from the session |
| `next-pane` | Pane |  | Cycle focus to the next pane |
| `previous-pane` | Pane |  | Cycle focus to the previous pane |
| `last-pane` | Pane |  | Jump back to the previously focused pane |
| `toggle-zoom` | Pane |  | Zoom the focused pane to fill the window (toggle) |
| `toggle-sidebar` | View |  | Show or hide the window sidebar (toggle) |
| `command-palette` | — |  | Open the command palette |
| `context-menu` | Pane |  | Open the context menu for the focused pane (ADR-0058) |
| `window-picker` | Window |  | Pick a window from all sessions (grouped) |
| `session-picker` | Session |  | Pick a session from a filterable list |
| `agent-fleet` | View |  | Agent fleet: every pane's agent, state, and attention |
| `focus-pane` | — | `window` (window index), `pane` (DFS leaf ordinal) | Focus a pane by window index and DFS leaf ordinal |
| `next-attention` | Pane |  | Jump to the next pane waiting for an answer |
| `return-from-attention` | Pane |  | Return to where attention navigation started |
| `switch-session` | — | `name`; `window?` (window index to select after the switch); `pane?` (DFS leaf ordinal to focus in that window) | Re-attach this client to another session |
| `new-session` | Session | `name?` (bare opens an interactive prompt) | Create a new session and switch to it |
| `take-input` | Pane |  | Take the wheel: seize exclusive input over the focused pane (ADR-0033) |
| `give-input` | Pane |  | Give back the wheel: release the focused pane's input lease (ADR-0033) |
| `signal-terminal` | Pane | `signal` = `interrupt` \| `freeze` \| `resume` \| `terminate` \| `kill` | Signal the focused pane's process group (freeze/resume/kill, ADR-0033) |
| `set-pane` | Pane | `mouse` = `on` \| `off` \| `toggle` | Toggle per-pane mouse opt-out for the focused pane (ADR-0048) |
| `plugin-action` | — | `plugin`, `action` | Run an enabled plugin's manifest action |
| `plugin-pane` | — | `plugin`, `pane` | Open an enabled plugin's manifest pane |
| `reload-config` | View |  | Reload the config file (keybindings, theme, status bar) |

Why the dash rows have no palette entry:

- `command-palette` — opening the palette from the palette is noise.
- `select-window` — parameterized by `index`, which the palette has no UI to collect; the window picker is the surface for "jump to window N".
- `switch-session` — requires a `name` arg supplied by the session picker (or the fleet's foreign rows), so a bare palette row would have no target to act on.
- `copy-mode` — a modal input surface entered from its keybinding, not a one-shot command the palette can commit.
- `plugin-action` — its palette rows are built dynamically from enabled plugins' manifests, one per manifest action, carrying `plugin`/`action` args a static row could not supply.
- `plugin-pane` — same shape as `plugin-action`: dynamic rows from enabled plugins' manifest `[[panes]]`, carrying `plugin`/`pane` args.
- `focus-pane` — parameterized by coordinates only the agent-fleet dashboard's rows can supply (the `select-window` precedent).
