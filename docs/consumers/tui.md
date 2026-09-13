---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-09-13
---

# The phux reference TUI

**TL;DR.** The reference TUI is the human attach client: split panes, switch
windows, detach, and return to the same live terminals. This page is that
product — prefix keys, layout, chrome, copy-mode, and the fleet overlay —
so a person can use it without reading the wire spec. Headless verbs and
other clients are peers; they cannot steal this client's focus.

---

## What this is

The TUI is the attach client that ships in the `phux` binary. `phux` with
no arguments starts the per-user server if needed, attaches, and paints
the session. Detach leaves the session running. A second terminal, a
script, Cockpit, or an agent can observe and drive those same terminals
while you watch.

This page is the TUI's product surface. The CLI verb catalog lives in
the [generated CLI reference](../reference/cli.md) and
[agents](./agents.md). Config keys live in
[configuration](../CONFIG.md). Recording lives in
[recording](./recording.md). Maturity lives in
[concepts](../CONCEPTS.md).

The TUI has no protocol-level standing
([ADR-0017](../adr/0017-tui-not-protocol-privileged.md)). Sessions,
windows, splits, the status bar, and keybindings are this client's
vocabulary, not the wire's.

## First minutes

Install, then:

```sh
phux
```

Work in it like a normal terminal. The default prefix is `Ctrl-A`. The status
bar also exposes the main destinations as clickable labels. These continuations
are enough for a first run:

| Keys | Action |
|---|---|
| `C-a s` | Sessions & hosts |
| `C-a S` | Settings |
| `C-a ?` | Commands and help (the same overlay as `C-a :`) |
| `C-a %` | Split left and right |
| `C-a "` | Split top and bottom |
| `C-a d` | Detach; the session keeps running |

Run `phux` again to reattach. The full first-run path, including driving
the same pane from a second terminal, is [`../QUICKSTART.md`](../QUICKSTART.md).

On the first attach for a profile, a compact overlay explains that the
session outlives the view, names the Agents list as the fleet inbox, and
shows the blocked (`●`) and working (`◐`) glyphs next to the live bindings
for detach, Sessions & hosts, Commands, Settings, and copy mode. If the
sidebar is off, the overlay names the live `toggle-sidebar` chord instead of
assuming the list is already on screen. It also points out that Shift-drag
uses the host terminal's selection. The first key dismisses it and still does
what that key normally does. After the first intentional detach, the cooked
terminal prints that the session is still running; the next attach shows a
brief status-bar confirmation. Later attaches are quiet. The palette's
**Getting started** row reopens the introduction without changing
progress.

A reader attached inside `phux` cannot run headless verbs in that TTY.
Open a second terminal for `phux ls`, `phux snapshot .`, `phux send-keys`,
and `phux wait`.

## User model

Three nouns. Same as tmux.

- **Session** — named container. Persists across detach. Lives until you
  kill it or the server exits. A session marked keep-empty can outlive
  its last window; the TUI stays attached and paints an empty state.
- **Window** — tab within a session. Numbered from 0; optionally named.
- **Pane** — leaf in a window's layout. One PTY, one terminal grid, one
  shell or command.

A **client** is an attached frontend. Clients are transient. Focus,
copy-mode, the sidebar toggle, and attention navigation are this client's,
not shared state.

An **agent session** is not a pane. It is a child resource of a pane: the
TUI never tiles it, never gives it a layout slot, and has no keybinding
that selects it. Sidebar and fleet rows read it. Closing the pane closes
its sessions; closing a session leaves the pane.

`phux worktree` binds a git checkout to a session whose name is derived
from the worktree path. That is a CLI composition; attaching to the
derived name is ordinary TUI attach. See [`agents.md`](./agents.md) for
the verbs.

## Selectors

A selector names a session, window, or pane in CLI arguments, keybinding
actions, and hooks. The server never parses one; the client resolves it
against a snapshot.

| Selector | Meaning |
|---|---|
| `.` | the focused pane, window, or session |
| `name` | session by name |
| `name:N` | session `name`, window index `N` |
| `name:N.M` | session `name`, window `N`, pane index `M` |
| `name:tag` | session `name`, window whose name is `tag` |
| `@N` | opaque local id, stable for the server's lifetime |
| `host/@N` | opaque id on federation satellite `host` |
| `%name` | the AgentSession named `name`, or its parent Terminal; refuses rather than guesses |
| `=` | attached TUI only: previous pane (`C-a =`) |
| `#tag` | every Terminal carrying L3 tag `tag` |

`=` is TUI-only. Headless CLI and MCP reject it: they have no focus
history, so an explicit `=` is an error rather than a silent alias of
`.`. In the attached TUI, `C-a =` is `last-pane` against a one-entry,
process-local MRU; repeating it toggles between two panes, including
across windows. The MRU is neither persisted nor sent on the wire.

`%name` yields exactly one agent or refuses. A Terminal-facet verb acts
on the parent pane; a session verb acts on the AgentSession.

`host/@N` is the federation form. A hub lists satellite terminals next
to local ones and does not merge remote session or window models. Attach
the satellite itself with `phux attach --remote HOST SESSION` when you
want that host's own windows and splits.

A selector that names several panes (a whole session or window) resolves
to one selected pane: the focused pane if it is among the matches, else
the first in snapshot order.

```sh
phux kill work:edit.2          # second pane in window "edit" of session "work"
phux send-keys @42 "ls" Enter  # local pane 42
phux snapshot devbox/@7        # satellite pane 7 through the hub
# `phux kill =` errors: headless clients have no focus MRU
```

## Keys

Two binding tables, both always present:

- **Prefix table** (`[keybindings.prefix-table]`): after the prefix. This
  is the tmux-shaped model. Default prefix is `C-a`.
- **Global table** (`[keybindings.global]`): any time. Empty by default;
  reserved for chords the outer terminal actually forwards (`super`,
  `hyper`, `meta`).

Bindings invoke named **actions**, not shell strings. The command
palette, pickers, sidebar clicks, and context menus commit the same
action a keybinding produces. The generated catalog is
[`../reference/actions.md`](../reference/actions.md); `C-a ?` shows the
live chords.

At attach, a bad binding disables exactly that binding: a chord that
fails to parse, or a sequence that is a strict prefix of another (the
later one in table-key order loses), is skipped. Everything else,
including `detach`, keeps working. Each skip is named on the status-bar
error line and points at `phux config check`. A `prefix` string that
fails to parse falls back to `C-a`. Reload is the exception: it is
all-or-nothing, because a reload has a previous good config to keep.

### Cheat sheet

Default prefix `C-a`. Override it in one line of config.

| Chord | Action |
|---|---|
| `C-a "` | `split-pane` horizontal (stacked) |
| `C-a %` | `split-pane` vertical (side-by-side) |
| `C-a x` / `C-a X` | `kill-pane` / `kill-window` |
| `C-a h/j/k/l` | `focus-direction` left/down/up/right |
| `C-a o` / `C-a ;` | `next-pane` / `previous-pane` |
| `C-a =` | `last-pane` (jump back; repeat to toggle) |
| `C-a z` | `toggle-zoom` |
| `C-a b` | `toggle-sidebar` |
| `C-a [` | `copy-mode` |
| `C-a c` | `new-window` |
| `C-a n/p` | `next-window` / `previous-window` |
| `C-a 0`–`9` | `select-window` by index |
| `C-a G` | `go-to-directory` |
| `C-a w` | `window-picker` |
| `C-a s` | `session-picker` (`C-a a` is a kept alias) |
| `C-a A` | `agent-fleet` |
| `C-a S` | `settings` |
| `C-a q` / `C-a Q` | `next-attention` / `return-from-attention` |
| `C-a C` | `new-session` |
| `C-a ,` / `C-a $` | `rename-window` / `rename-session` |
| `C-a H/J/K/L` | `resize-pane` by 5 |
| `C-a :` / `C-a ?` | `command-palette` / `show-help` (one overlay) |
| `C-a d` | `detach` |

### Which-key

Press the prefix and hesitate, and a small panel lists every prefix-table
continuation, built from the live bindings. Numeric window-jump keys
collapse into one `0-9` row. The popup is display-only: any key dismisses
it and executes as if it had never appeared. A continuation typed before
the delay suppresses it entirely. Esc dismisses it and cancels the
pending prefix.

```toml
[keybindings]
which-key = true          # default
which-key-delay-ms = 400  # default
```

## Layout

A window's layout is a **binary split tree**: each interior node is a
horizontal or vertical split with a ratio in `(0, 1)` and exactly two
children; leaves are panes. Three-way splits are nested binary splits.

Panes **share** their rules. A split costs one cell of chrome, not two
adjacent borders, so a 2×2 window is one `│` column and one `─` row
crossing at a `┼`. Above the pane area sits the **rail**: one reserved
row that closes the grid at the top and holds each top-row pane's title.
Splitting a window never moves the panes you were already looking at.

Focus is colour, not a heavier stroke: `divider_focus` plus bold on the
focused pane's rules and title, `divider` everywhere else. A pane's title
is its own OSC-2 terminal title. A pane whose program never set a title
gets no label. Control characters and explicit bidi overrides are dropped
from every chrome label. A pane that has asked for a human badges with a
filled `●` in the `attention` tone ahead of its title — the same glyph
the sidebar uses for that pane.

On viewport resize, split ratios are preserved and space redistributes
proportionally. A leaf that hits its minimum (`min_cols = 2`,
`min_rows = 1` for inner content) freezes; remaining space goes to
non-frozen leaves. Below the layout's aggregate minimum, freezing
disengages and panes degrade to sub-viable rectangles rather than
disappearing. `C-a H/J/K/L` moves the boundary against the focused pane's
neighbor by changing that node's ratio. A resize that would push either
side below 2 cells on that axis is a bell-no-op.

**Shared geometry.** A Terminal has one `(cols, rows)`. Concurrent views
letterbox or crop rather than reflowing a second grid.
`defaults.window-size` picks the policy: `smallest` (default; nothing is
cropped), `largest`, `latest`, or `manual`. An explicit `phux resize`
applies immediately; under every policy but `manual`, the next view
event recomputes and supersedes it. `manual` is the setting for a
scripted geometry.

**Satellite splits.** With a satellite pane focused, `split-pane` opens
the new pane on that same satellite, through the hub. The split appears
once the new pane attaches; a refused spawn leaves no dead split and
tries to kill the spawned pane. Against a hub that cannot spawn there,
the split opens on the hub and a notice says so.

## Status, sidebar, and theme

### Status bar

The bar is one reserved row of the outer terminal, client-side, default
**top** (`[status] position = "bottom"` moves it). Contents are lists of
widgets:

```toml
[status]
left   = [{ kind = "windows" }]
center = [{ kind = "help-hints" }]
right  = ["session-name", { kind = "time", format = " %H:%M" }]
position = "top"
```

A bare string is a no-parameters widget. The generated catalog is
[`../reference/widgets.md`](../reference/widgets.md). Plugin manifests
may append widgets after the user's own; a contribution that fails
validation is dropped with a warning.

When the three slots want more than the row, **right** takes up to half,
**left** (the tab strip) gets the rest, **center** gets the surviving
gap. Within a slot, later widgets yield first. Widgets drop whole units,
never fragments: `windows` drops whole tabs around the active one;
`help-hints` shows Sessions, Commands, Settings, Help, and Copy. Each complete
label is a click target for the same action as its keybinding; the prefix and
separators are inert. It drops whole hints from the right, leaving Sessions as
the last route on a tight bar. `min-cols` / `max-cols` hide a widget
outright. The shipped lineup uses that to change shape at 64 columns:
session name and clock give way to a clickable `switch` chip that opens
the fleet dashboard.

### Spacer

A `spacer` widget has no content and absorbs leftover columns. Slack is
row-wide: every spacer splits the same leftover width. A bar with a
spacer has no room left for the center slot. Spacers yield first on a
narrow terminal, so they cannot push content off the screen.

The bar is not multi-row and not a styling engine. Per-widget `style`
tables only.

**Asked chrome.** When an agent in a pane blocks for a human, the asking
window gets a ` !` suffix on its tab, and a right-aligned `[ ASK ]`
chip appears on the bar (`[ ASK xN ]` for several). `C-a q`
(`next-attention`) jumps to the next asking pane in window then
depth-first leaf order, wrapping; the first jump saves where you came
from. `C-a Q` (`return-from-attention`) returns there once. Both are
client-local: they send no frame and write no shared focus. The CLI
cannot move this viewport. Attention clears when you focus the pane
**and type or paste**; merely focusing does not. The flag is per-attach
and does not persist across detach.

**Notices.** Lifecycle events take the bar row for about seven seconds,
newest-wins: input-lease handovers on the focused pane, a satellite
becoming unreachable, a pane dying with a non-zero exit (clean `exit 0`
and a kill you requested are silent), and re-attach after a server
restart. An empty `[status]` reserves no row, so notices degrade to log
lines. When the last pane of a default session dies, the TUI tears down
and prints one cooked-terminal line naming the exit. A keep-empty
session stays attached and paints `Empty session` with the `new-window`
chord.

**Reconnect.** If the server vanishes mid-session, the TUI drops to the
cooked screen and waits: 10 seconds, polling every 100 ms, on the local
socket; 60 seconds with exponential backoff on `--ws` / `--quic`. A
clean shutdown unlinks the socket and the client stops immediately. A
timeout names the server log and `phux doctor`. Keystrokes in the drop
are not replayed. On remote lanes against a server that advertises
`ACKNOWLEDGED_INPUT`, a paste in flight is resent under the same
idempotent id or reported as unknown / not delivered, never silently
doubled.

### Sidebar

`[sidebar]` docks a vertical strip on the left (default) or right. It is
**on by default**. `C-a b` (`toggle-sidebar`) flips it for the life of
the attach; `[sidebar] enabled` seeds that choice at attach only. Panes
tile into the remaining content rect. Default `width = 0` sizes
automatically: a quarter of the viewport, bounded to 28–40 columns. A
positive width is exact. Automatic width depends only on viewport size,
so changing titles never reflows work.

The strip runs the full height of the terminal. The status bar yields
its columns rather than spanning underneath. After two footer rows, the
upper half is **Agents** and the lower half is **Sessions**. The split
depends only on viewport height.

**Agents** lists agent rows in session / window / pane order. A filled
dot (`●`) is blocked on you; a half-filled ring (`◐`) is still working.
When none are running, the list says `none running yet`. Status updates
in place; the list does not sort by urgency. A local row selects that
window; a peer row is a one-step `switch-session` onto that pane.
Overflow is a `+N more` row that opens the fleet dashboard.

<!-- impl-status: shipped; probe: AgentSessionRow -->
> **Status: shipped.** When a pane has a live agent session, the sidebar
> and fleet rows take state from that stream (provider as the kind,
> stream-derived glyph). Otherwise they use the `phux.agent/v1` record,
> then the OSC-title heuristic. Older servers that do not advertise
> `RESOURCE_KINDS` have no session stream; `phux status --json` is the
> check. An agent session never earns a row of its own.

**Sessions** lists every known session on this server, then
host-qualified satellite sessions. The current session expands its
windows. A satellite session shows a pane count and `?`, because its
per-terminal metadata is not subscribable from here.

Click targets commit the same actions as keys. The **Agents** and **Sessions**
headings open their full management views; window and roster rows select their
destination; overflow opens the matching view. The footer keeps `+ new window`
on one row and `= commands  S settings` on the next, with an independent target
for each action. The collapse chevron runs `toggle-sidebar`. Pointer events over
the strip never leak into pane routing.

### Small terminals

A viewport is **compact** on an axis at or below 64 columns or 18 rows,
judged independently. Overlays go full-bleed on the starved axis (still
stopping at a docked sidebar). List rows yield their secondary column
before the label, then clip with `…`. The sidebar is not reserved below
resolved sidebar width + 40 columns; `C-a b` rings the bell at those
widths rather than flipping a flag with no visible effect. Turning the
strip off is always allowed.

```toml
[chrome]
compact-cols  = 64
compact-rows  = 18
min-pane-cols = 40
```

`0` disables a threshold. `[chrome]` does not reach into `[status]`: the
shipped bar's shape change at 64 columns is per-widget `min-cols` /
`max-cols` in your config. Change both if you want them to agree.

### Theme

`[theme]` is a free-form `slot = color` map for chrome and overlays.
Unknown slot keys are ignored; an unparseable color keeps that slot's
default. Colors accept names (`"cyan"`), hex (`"#cdd6f4"`), and ANSI
indices (`"12"`). `phux config show --default` prints the shipped slots;
[`../CONFIG.md`](../CONFIG.md) owns the file.

```toml
[theme]
accent = "#cdd6f4"
attention = "#fde047"
surface = "#171b23"
```

## Copy-mode

`C-a [` enters copy-mode on the focused pane. Copy-mode is
**client-local**: a projection over the pane's own libghostty engine.
Nothing about a selection touches the wire. The client extracts the
selected text from its own `Terminal` and writes it to the host clipboard
via OSC 52
([ADR-0045](../adr/0045-client-side-copy-mode.md)).

- Arrow keys move the cursor; hold Shift to extend from the anchor.
- An arrow past the edge, and PageUp / PageDown, scroll the client-local
  viewport into mirrored scrollback. Selection is bounded by the
  scrollback this client already holds, not the server's full history.
- **Tab** rotates Char (linear) → Line → Rect (block). Highlight and
  extracted text come from the same rectangle.

One-shot grabs copy-and-exit:

| Key | Grab |
|---|---|
| `w` | word under the cursor |
| `v` | whole line |
| `V` | line bounded by OSC-133 prompt changes |
| `A` | all selectable content |
| `]` | command-output span; no-op when the pane has no OSC-133 zones |

Enter copies the current selection and exits. Esc exits without copying.
A left-button drag inside the pane selects and, on release, copies and
exits; a click with no drag exits, so a mouse-initiated entry cannot
trap the keyboard. The wheel scrolls the local viewport. Resizing the
terminal **keeps** copy-mode open and adopts the new size.

## Command palette, pickers, and settings

`C-a :` (`command-palette`) and `C-a ?` (`show-help`) are two aliases
for one filterable **Commands & Help** overlay. Every action is annotated
with its currently-bound chord. Empty query: rows grouped under Pane,
Window, Session, View. Typing ranks a fuzzy match; Enter commits through
the same dispatcher a keybinding uses. Navigate with arrows / `C-n` /
`C-p` (`j` / `k` while the query is empty), PageUp / PageDown, Home /
End, or the wheel. Enabled plugin `[[actions]]` and hostable `[[panes]]`
appear under a trailing **Plugin** header.

<!-- impl-status: partial; probe: PluginPanePlacement -->
> **Status: partial.** Manifest `placement = "overlay"` is valid schema
> and is skipped with a logged warning. `split`, `tab`, and `zoomed`
> open a real server-side Terminal.

The **Sessions & hosts** view (`C-a s`) lists other sessions; choosing one
re-attaches this client in-process. A trailing "+ New session" row
creates one. Against a federation hub the view is grouped by host and refreshes
in place as inventory changes. A reachable host shows its session count, an
empty reachable host says `connected, no sessions`, and an unreachable host
keeps its diagnostic visible. A
satellite row cannot re-attach this client to that remote session:
`ATTACH` is not federation-routable. Choosing it opens that session's
active pane as a window of the session you are already in. The window
holds the satellite's real Terminal; closing it kills that pane there.

The **window picker** (`C-a w`) is hierarchical: sessions as headers,
windows nested. A window in the current session switches directly; a
window in another session is a one-step `switch-session` that also
selects that window.

**Move the focused pane** is available under Pane in Commands & Help and as
**Move beside…** in the pane context menu. It opens one fuzzy list of exact
local destination panes from the current layout and every fully cached
session layout. Rows show the stable `@id`, session, window index/name, and
pane number. The focused pane, satellite panes, and sessions whose layout is
not cached are not offered; if nothing is eligible, the action bells without
changing anything. Enter moves the existing Terminal beside the selected pane
side-by-side at ratio `0.5`; Esc cancels. The process, scrollback, metadata,
agent record, subscriptions, and Terminal id stay attached to that identity.
Focus follows it, including an in-process reattach when it crosses sessions.
Move, layout, and rollback failures stay in the TUI as a **Pane move failed**
message rather than silently changing or ending the attach.

The **directory picker** (`C-a G`) browses directories on the attached
server and opens a new window there. Over `phux --remote` it browses the
remote host. With a satellite pane focused, and a hub that advertises
`LIST_DIRECTORY_HOST`, it lists that satellite through the hub.

### Settings page

`C-a S`, the status-bar Settings label, or the sidebar footer opens the config
as a page: sections down the left, keys on the
right, a detail panel underneath. Each row shows the effective value and
where it came from (`default`, `you`, or an `extends` layer). Editing
writes **your** `config.toml` one key at a time, comments intact, then
requests the same in-place reload as `reload-config`. The page never
writes running state: `C-a b` toggling the sidebar does not touch the
file; editing `sidebar.enabled` here does.

Enter or Space toggles a bool or opens the inline editor. Left / Right
cycle a choice or step an integer. Del or `C-r` removes your override.
`C-z` undoes the last edit made on this page. A refused edit names its
reason and leaves the file alone. Composite settings (widget lists,
binding tables, hooks, plugin and host registries) stay in the file; the
page tells you where it is.

A change lands **now** for anything a reload covers, **next attach** for
`[sidebar]`, `defaults.mouse`, and `experimental.*`, and **next server
start** for `[defaults]` and `[voice]`.

## Agent fleet

`C-a A` (`agent-fleet`) is the one-view answer to which agent needs you:
a filterable overlay of every pane of the attached session, grouped
under session headers. Each row carries the agent's name and kind, a
state glyph (`!` blocked, `*` working, `-` idle, `.` done, `?` unknown),
an attention highlight when the pane has a pending question, and branch
or cwd in the dim right column.

Enter focuses the chosen pane. Rows under other sessions are one-step
cross-session focus when that peer's layout is cached; otherwise a
single "switch to this session" row. Foreign rows carry no asked flag or
branch — those need a live subscription. The dashboard is live: while it
is open, record changes, asks, spawns, and layout changes rebuild rows
in place without disturbing the query. `phux agent list` remains the
exhaustive cross-session CLI projection.

## Mouse

Mouse handling is on by default. On attach the client enables button-event
tracking plus SGR coordinates on the *outer* terminal and restores them
on detach, so divider drags work in a plain shell.

| Event | Action |
|---|---|
| Click in a pane | Focus, then forward |
| Press / drag a divider | Resize; release commits the layout |
| Wheel in a pane | Inner mouse mode gets the wheel; else primary screen scrolls local scrollback, alt screen becomes arrows |
| Right-click in a pane | Pane context menu, unless the inner program has mouse tracking |
| Click a status-bar tab | `select-window` |
| Click a status-bar destination | Open Sessions, Commands, Settings, Help, or Copy |
| Click a sidebar row | The same action the keyboard binding would run |

Hold **Shift** to bypass application mouse reporting and use the host
terminal's native selection. `mouse = false` in `[defaults]` skips
capture entirely. Per-pane, `set-pane mouse off` (palette toggle) drops
this client's mouse handling while that pane is focused; a click on it
still focuses it, which is the path back in.

Right-click opens a menu for the pane, the window, or the session,
listing the actions that apply. The session menu includes Sessions & hosts,
Agent fleet, Settings, and Commands & Help. Each row commits the same action a
keybinding would. An inner program with mouse tracking on keeps every
button, so no menu opens over it; bind `context-menu` for the keyboard
path. A terminal resize closes the menu; other overlays reflow.

## Config and reload

phux is config-driven: one TOML file, never written back from running
state. There is no `set-option`. Defaults live inside the binary; your
`config.toml` is a sparse overlay. A missing file is not an error.

```
phux config path     # resolved path, no I/O
phux config init     # commented starter; refuses to overwrite
phux config show     # effective config as canonical TOML
phux config check    # every unknown key and wrong value, with dotted path
phux config reload   # validate, then apply to running clients
```

The file is `$XDG_CONFIG_HOME/phux/config.toml` (else
`~/.config/phux/config.toml`). Schema, layers, widgets, and hooks:
[`../CONFIG.md`](../CONFIG.md). Annotated defaults:
`phux config show --default` and
[`../reference/config.md`](../reference/config.md).

### Reloading

Reloads are explicit, never automatic. Three surfaces trigger the same
in-place reload:

- the `reload-config` action (palette row "Reload the config file";
  unbound by default)
- a committed edit on the settings page
- `phux config reload` from any shell, which rings every attached client

A reload rebuilds keybindings, the theme, the status-bar composition,
and plugin palette rows, atomically. On any parse or validation error
the previous config stays fully in effect and a dismissable toast names
the error. The file is not watched. Not covered by a reload (detach and
re-attach): `[sidebar]` geometry, `[experimental]`, and `[defaults]`
(the server owns those last).

`[experimental] predictive-echo` is unset by default: prediction is on
for a remote attach that actually leaves the machine, off on the local
socket and on loopback `--quic` / `--ws`. Set `true` or `false` to
override. The overlay is a local paint; it never reaches the wire,
another client, or a recording.

## Hooks

Hooks fire at named server events. Config parsing and the dispatcher live
with the rest of the file in [`../CONFIG.md`](../CONFIG.md). The TUI
does not play sounds or post desktop notifications; `agent-state-changed`
is the edge a notifier hook should match.

Shipped events: `after-new-pane`, `pane-exit`, `focus-changed`,
`client-attached`, `client-detached`, `agent-state-changed`. First match
wins per event. Actions are child processes, fire-and-forget, bounded.
Every hook child gets `PHUX_EVENT`, `PHUX_SOCKET`, and one `PHUX_*`
variable per context key.

```toml
[[hooks.agent-state-changed]]
when   = { to = "blocked" }
action = { kind = "run", command = "afplay /System/Library/Sounds/Glass.aiff" }
```

`from` is absent on a first sighting. A withdrawn record arrives as
`to = "unknown"`.

## Where to go next

| You want | Read |
|---|---|
| Install and the first attach | [Quickstart](../QUICKSTART.md) |
| Every config key | [Configuration](../CONFIG.md) |
| Every action the dispatcher handles | [Action catalog](../reference/actions.md) |
| Every status-bar widget | [Widgets](../reference/widgets.md) |
| Headless verbs, JSON, `%name` | [Agents](./agents.md) |
| Record a pane or the glass | [Recording](./recording.md) |
| The native macOS client | [Cockpit](./cockpit.md) |
| What is shipped versus a gap | [Concepts](../CONCEPTS.md) |
