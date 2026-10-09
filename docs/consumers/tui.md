---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-10-09
---

# The phux reference TUI

**TL;DR.** The TUI attaches to server-owned terminals: split panes, switch
windows, detach, and return while work keeps running. Use prefix keys, the
command palette, or pointer controls to manage layout and inspect agent
state. Focus and copy-mode stay local to this client; headless verbs and
other clients cannot move its focus.

---

## What this is

Running `phux` with no arguments starts the per-user server if needed and
attaches the TUI. Detaching leaves the session running on that server.

For headless commands, see the [CLI reference](../reference/cli.md) and
[agent guide](./agents.md). [Configuration](../CONFIG.md),
[recording](./recording.md), and [product maturity](../CONCEPTS.md) have
separate guides.

The TUI has no protocol-level standing
([ADR-0017](../adr/0017-tui-not-protocol-privileged.md)). Sessions,
windows, splits, the status bar, and keybindings are this client's
vocabulary, not the wire's.

## First minutes

Install, then:

```sh
phux
```

The default prefix is `Ctrl-A`: press it, release both keys, then press the
continuation. Use clickable window tabs, the command palette (`C-a Space`
or `C-a :`), or right-click menus for other actions. Start with:

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

The first attach for a profile shows a short overlay of the live bindings
and agent glyphs; the first key dismisses it and still does what it
normally does. The palette's **Getting started** row reopens it.

A reader attached inside `phux` cannot run headless verbs in that TTY.
Open a second terminal for `phux ls`, `phux snapshot .`, `phux send-keys`,
and `phux wait`.

## User model

The TUI uses three familiar multiplexer terms:

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

`phux attach NAME` joins an existing session on a running server. An unknown
name exits 1 and suggests `phux new NAME`. If no server is running, auto-start
seeds the first session under NAME.

`phux attach --viewer` renders every pane but refuses this attach's input.
`phux attach --take` takes input authority for every pane it opens; the
previous holder stays attached and sees a handover notice. Plain
`phux attach` requests neither role. Both flags require a server advertising
attach roles and are refused by older servers (ADR-0127). A viewer's viewport
still sizes panes, but it cannot answer terminal queries, so applications
waiting for replies time out.

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

A session name must read back as itself: `new`, `rename`, and the TUI
prompts refuse an empty name, `.` and `=`, a leading `@`, `#`, or `%`,
and a `:` or `/@` anywhere in it.

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

While attached, the TUI asks the outer terminal for the kitty keyboard
protocol (disambiguate only, [ADR-0146](../adr/0146-tui-pushes-kitty-keyboard-disambiguate-on-the-host.md)).
Modified keys such as Cmd+Return, Ctrl+I, and Ctrl+Backspace then reach a pane
exactly as the outer terminal would deliver them natively. A terminal without
the protocol ignores the request and phux decodes its legacy keys. Caps Lock
and Num Lock never affect chord matching.

Bindings invoke named **actions**, not shell strings. The command
palette, pickers, sidebar clicks, and context menus commit the same
action a keybinding produces. The generated catalog is
[`../reference/actions.md`](../reference/actions.md); `C-a ?` shows the
live chords.

At attach, an invalid chord, unknown action, or ambiguous sequence disables
only that binding. If a sequence is a strict prefix of another, the later
binding in table-key order loses. The status-bar error names each skipped
binding and points to `phux config check`; other bindings keep working.
An invalid `prefix` falls back to `C-a`. Reload is all-or-nothing and retains
the previous configuration on failure.

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
| `C-a <` / `C-a >` | `move-window` one slot left / right |
| `C-a G` | `go-to-directory` |
| `C-a F` | `find-path` (browse or search paths on the focused pane's host; `insert-path` types one shell-quoted path, never Enter) |
| `C-a w` | `window-picker` |
| `C-a s` | `session-picker` (`C-a a` is a kept alias) |
| `C-a A` | `agent-fleet` |
| `C-a S` | `settings` |
| `C-a B` | `report-bug` (local bug-report bundle) |
| `C-a q` / `C-a Q` | `next-attention` / `return-from-attention` |
| `C-a C` | `new-session` (focused pane's host and directory, or `cwd` / `host`) |
| `C-a N` / `C-a P` | `next-session` / `previous-session` |
| `C-a )` | `last-session` |
| `C-a ,` / `C-a $` | `rename-window` / `rename-session` |
| `C-a H/J/K/L` | `resize-pane` by 5 |
| `C-a :` / `C-a ?` | `command-palette` / `show-help` (one overlay) |
| `C-a d` | `detach` |

### Which-key

Pause after the prefix to see a panel of live continuations; numeric window
jumps share one `0-9` row. Any continuation dismisses the panel and runs
normally. Typing before the delay prevents it from appearing. Esc dismisses
it and cancels the pending prefix.

```toml
[keybindings]
which-key = true          # default
which-key-delay-ms = 400  # default
```

## Layout

A window's layout is a **binary split tree**: each interior node is a
horizontal or vertical split with a ratio in `(0, 1)` and exactly two
children; leaves are panes. Three-way splits are nested binary splits.

Panes share dividers, so each split costs one cell rather than two adjacent
borders. A 2×2 window has one `│` column and one `─` row crossing at `┼`.
The rail reserves a row above the pane area for top-row pane titles.
Splitting a window never moves the panes already displayed.

Focused dividers and titles use `divider_focus` and bold; other dividers use
`divider`. Titles come from OSC-2 and remain blank until the program sets one.
All chrome labels discard control characters and explicit bidi overrides.
A pane requesting attention gets a filled `●` in the `attention` color
before its title, matching the sidebar.

On viewport resize, split ratios are preserved and space redistributes
proportionally. A leaf that hits its minimum (`min_cols = 2`,
`min_rows = 1` for inner content) freezes; remaining space goes to
non-frozen leaves. Below the layout's aggregate minimum, freezing
disengages and panes degrade to sub-viable rectangles rather than
disappearing. `C-a H/J/K/L` moves the focused pane's own boundary left,
down, up, or right by that many cells: the divider of the nearest
enclosing split on that axis, whichever side the focused pane is on
(tmux's `resize-pane`). A resize that would push either side below 2
cells on that axis is a bell-no-op.

**Shared geometry.** A Terminal has one `(cols, rows)`. Concurrent views
letterbox or crop rather than reflowing a second grid. The TUI sizes each
pane to its own tile, and its cell pixel size rides along. Against a server
that supports this, the TUI casts no session-wide size vote, so attaching,
switching sessions, or resizing the outer window never sends a pane through
the full window size on the way to its tile
([ADR-0145](../adr/0145-the-tui-sizes-panes-and-casts-no-viewport-vote.md)).
When two such TUIs view one pane, the last tile sent wins. For clients that
vote, `defaults.window-size` picks the policy: `smallest` (default; nothing is
cropped), `largest`, `latest`, or `manual`. An explicit `phux resize`
applies immediately; under every policy but `manual`, the next view
event recomputes and supersedes it. When the last usable voting view
detaches, those automatic policies return the live Terminal to the usable
headless geometry, 80 columns by 24 rows. A vote-free TUI's detach leaves
its tiles in place, so a session switch resizes nothing, unless it leaves an
unwatched pane under 10x3, which also returns to 80x24. Either way a tiny
viewport cannot strand a durable shell at 1x1. `manual` is the setting for a scripted geometry
and holds an explicit size across detach.

**Satellite splits.** A window may mix local and satellite panes. With a
satellite pane focused, `split-pane` opens on that satellite through the hub;
`split-pane` with `host` spawns there from a local pane, and with `resource`
(`host/@N` or `@N`) attaches an existing pane. A refused spawn leaves no dead
split. A satellite pane's border and the status bar name its host; while the
hub reports it unreachable the pane stays, greys out, and drops keys, and it
reattaches into the same slot when the satellite returns.

## Status, sidebar, and theme

### Status bar

The bar is one reserved row of the outer terminal, client-side, default
**top** (`[status] position = "bottom"` moves it). Contents are lists of
widgets:

```toml
[status]
left   = [{ kind = "windows" }]
center = []
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
never fragments: `windows` drops whole tabs around the active one.
`help-hints` is opt-in teaching chrome (Sessions, Commands, Settings, Help,
Copy); it is not in the shipped center slot. Each of its complete labels is
a click target for the same action as its keybinding. `min-cols` /
`max-cols` hide a widget outright. The shipped lineup uses that to change
shape at 64 columns: session name and clock give way to a clickable
`switch` chip that opens the fleet dashboard. A focused satellite pane
adds its host to the supervisory badge (`devbox`, or `devbox down` while
that satellite is unreachable).

### Spacer

A `spacer` widget has no content and absorbs leftover columns. Slack is
row-wide: every spacer splits the same leftover width. A bar with a
spacer has no room left for the center slot. Spacers yield first on a
narrow terminal, so they cannot push content off the screen.

The bar supports one row and per-widget `style` tables.

**Asked chrome.** When an agent in a pane blocks for a human, the asking
window gets a ` !` suffix on its tab, and a right-aligned `ask`
mark appears on the bar (`ask·N` for several). `C-a q`
(`next-attention`) jumps to the next asking pane in window then
depth-first leaf order, wrapping; the first jump saves where you came
from. `C-a Q` (`return-from-attention`) returns there once. Both are
client-local: they send no frame and write no shared focus. The CLI
cannot move this viewport. Attention clears when you focus the pane
**and type or paste**; merely focusing does not. The flag is per-attach
and does not persist across detach.

**Notices.** Lifecycle events appear in a compact right-aligned bar toast for
about seven seconds, newest-wins: input-lease handovers on the focused pane
(including a `take --ttl` lease that the server itself expired, ADR-0033 —
the TUI's ordinary subscription renders that the same as an explicit
`phux give`), a satellite becoming unreachable, a pane dying with a
non-zero exit (clean `exit 0` and a kill you requested are silent), and
re-attach after a server restart. An empty `[status]` reserves no row, so
notices degrade to log lines. A pane whose server history becomes
unavailable (pruned, tombstoned, or over the history limit) raises a
`scrollback unavailable` notice and keeps a `no-scrollback` token in the
supervisory badge while focused, until a fresh bootstrap restores history.
Scrollback this client already holds still scrolls.
When the last pane of a default session is killed, the TUI tears down and prints one
cooked-terminal line naming the exit. Natural `exit` of that last shell is
replaced in place by the server, so the attach stays on a fresh prompt.
A keep-empty session stays attached and paints `Empty session` with the
`new-window` chord after Close Tab of its last pane.

**Retained panes.** A pane spawned with retention (`--retain` or
`defaults.retain-on-exit`, ADR-0124) keeps its place and last screen after its
process exits. The sidebar and tabs mark it with a dim `x` plus status (`x3`,
`xsig9`), and the bar shows `[ exited N ]` while focused. Input is dropped;
scrollback and copy-mode still work. It closes when the server purges it or
you kill it.

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
its columns rather than spanning underneath. After the footer row, the
upper half is **Agents** and the lower half is **Sessions**. The split
depends only on viewport height.

**Agents** lists agent rows in session / window / pane order. A filled
dot (`●`) is blocked on you; a half-filled ring (`◐`) is still working.
When none are running, the list is a quiet em dash. Status updates
in place; the list does not sort by urgency. A local row selects that
window; a peer row is a one-step `switch-session` onto that pane.
Overflow is a `+N` row that opens the fleet dashboard.

<!-- impl-status: shipped; probe: AgentSessionRow -->
> **Status: shipped.** When a pane has a live agent session, the sidebar
> and fleet rows take state from that stream (provider as the kind,
> stream-derived glyph). Otherwise they use the `phux.agent/v1` record,
> then the OSC-title heuristic. Older servers that do not advertise
> `RESOURCE_KINDS` have no session stream; `phux status --json` is the
> check. An agent session never earns a row of its own.

**Sessions** is grouped by machine. Each machine gets a header row with
its sessions beneath it. The machine this terminal is attached to comes
first, marked `here`. The current session expands its windows. Next come
host-qualified satellite sessions, if this server is a hub. Each satellite
session shows a pane count and `?`, because its per-terminal metadata is
not subscribable from here. Last come your other machines: this one, when
you are attached somewhere else, and every host in `phux host ls`. A
machine that did not answer is marked `down`.

Clicking a session on another machine commits `switch-host`. That detaches
and re-attaches this terminal there, the same as running
`phux attach --remote HOST SESSION`. Other machines are re-listed every
`[sidebar] hosts-refresh-secs` (default 10) by a *hosts provider*: a command
that prints the `phux.hosts/v1` document. The default provider is
`phux ls --all --json`. Set `[sidebar] hosts-provider = ["cmd", "arg"]` to
supply your own, or `hosts = false` to list only the attached server
([ADR-0140](../adr/0140-sidebar-machines-come-from-a-hosts-provider.md)).
The session picker (`C-a s`) lists the same machines, one group each after
the attached server's sessions and any satellites; its filter matches a
session by its name or its machine's.

An enabled plugin can add **sections** with `[[sidebar]]` entries in its
manifest. Each is a band between Agents and Sessions: a header, then
exactly `rows` rows (default 3, at most 8), so a section filling or
emptying never moves Sessions. Rows are this session's panes in
window/leaf order, rendered from `format`; a pane gets a row only when
every token the format names resolves for it. An empty section shows the
quiet dash, and a full one ends in `+N`. Clicking a row focuses its pane.
Sections are drawn only while Agents and Sessions keep eight rows between
them; up to four are drawn, the last declared yielding first
([ADR-0148](../adr/0148-plugin-sidebar-sections-are-fixed-bands-of-pane-rows.md)).

| Token | Value |
|---|---|
| `{window}` | the window's tab label |
| `{index}` | the window's `select-window` index |
| `{title}` | the pane's OSC title |
| `{cwd}` | the pane's working directory, `$HOME` shown as `~` |
| `{exit}` | the last command's exit code (needs OSC-133 shell integration) |
| `{agent}` | the agent name from the pane's `phux.agent/v1` record |
| `{state}` | that record's state: `idle`, `working`, `blocked`, `done`, `unknown` |

```toml
# phux-plugin.toml
[[sidebar]]
id = "exits"
title = "Last exit"
format = "{window}: exit {exit}"
rows = 3
```

An unknown token fails `phux plugin validate` and plugin loading with a
did-you-mean suggestion.

Click targets commit the same actions as keys. The **Agents** and **Sessions**
headings open their full management views; window and roster rows select their
destination; overflow opens the matching view. The footer keeps `+ new window`
as the create affordance. Commands and Settings stay on the palette
(`C-a Space` / `:`) and the context menu. The collapse chevron runs
`toggle-sidebar`. Pointer events over the strip never leak into pane routing.

### Small terminals

A viewport is compact at or below 64 columns or 18 rows, independently on
each axis. Overlays fill the constrained axis except for a docked sidebar.
List rows drop secondary text before clipping labels with `…`; short
secondary text such as a bound chord stays whole if it fits within a third
of the row. The sidebar is hidden below its resolved width plus 40 columns.
At those widths, `C-a b` rings the bell instead of enabling it. Disabling
the sidebar is always allowed.

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

`[theme]` is a `slot = color` map for chrome and overlays, plus two
reserved keys that select a palette underneath the slots (ADR-0157):
`name` picks an installed theme (`phux theme list`) and `file` points at
any Omarchy-schema `colors.toml`. Unknown slot keys are ignored; an
unparseable color keeps that slot's default; a `name` that does not
resolve is a warning at attach and a refusal on `phux config reload`.
Colors accept names (`"cyan"`), hex (`"#cdd6f4"`), and ANSI indices
(`"12"`). `phux config show --default` prints the shipped slots;
[`../CONFIG.md`](../CONFIG.md) owns the file.

```toml
[theme]
name = "tokyo-night"
attention = "#fde047"   # an explicit slot still wins
```

A selected theme's semantic keys land on the slots like this. The
terminal's own ANSI palette is not touched by the TUI; that is the
client runtime's job and lands separately.

| theme key | slots |
| --- | --- |
| `accent` | `accent`, `title`, `divider_focus`, `pane_title_focus`, `agent_done` |
| `green` | `chord`, `agent_working` |
| `yellow` | `attention`, `agent_blocked` |
| `red` | `error` |
| `foreground` | `dim`, `section_header`, `sidebar_section`, `agent_idle`, `pane_title` |
| `bright_foreground` | `text` |
| `muted` | `border`, `divider` |
| `selection`, `selection_foreground` | `selection_bg`, `selection_fg` |
| `lighter_background` (dark) / `dark_background` (light) | `surface` |

`action` and `shadow` stay `reset`. The WCAG floor the shipped palette
pins is not promised for an arbitrary theme; `phux theme show` prints
the foreground-on-background ratio.

## Copy-mode

`C-a [` enters copy-mode on the focused pane. Selection and scrolling are
client-local, using this client's libghostty engine. Copying extracts text
from its `Terminal` and writes it to the host clipboard via OSC 52; selection
sends nothing over the wire
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

**Search.** `/` searches forward (toward newer output) and `?` backward;
the search line edits like the name prompts, Enter runs it, Esc closes it
without leaving copy-mode. The cursor jumps to the next hit after it,
wrapping at either end, and scrolls it into view; the hit becomes the
selection (so Enter copies it) and the other visible hits are underlined.
`n` repeats the search in its direction and `N` reverses it; an empty search
line repeats the last one. The strip shows `/needle 2/7` or `no match`.
Search is case-sensitive and covers the scrollback this client holds, like
selection.

Enter copies the current selection and exits. Esc exits without copying.
A left-button drag inside the pane selects and, on release, copies and
exits; a click with no drag exits, so a mouse-initiated entry cannot
trap the keyboard. As in Ghostty, a double-click copies the word under the
pointer, a triple-click the whole line, and an Alt-drag selects a rectangle
(block). Repeat clicks count when they land on the same cell within 500 ms.
The wheel scrolls the local viewport. Resizing the terminal **keeps**
copy-mode open and adopts the new size.

## Paste protection

A paste the focused pane would receive as typed input asks first, following
Ghostty's `clipboard-paste-protection`: when the pane's program has not
enabled bracketed paste (DEC 2004) and the text contains a line break (each
one would press Enter), or whenever the text contains the bracketed-paste
terminator `ESC [ 201 ~`. A modal names what would happen; Enter or `y`
delivers the paste, Esc or `n` drops it. Other pastes go straight through.

## Command palette, pickers, and settings

`C-a :` (`command-palette`) and `C-a ?` (`show-help`) open the same Commands
overlay. Actions show their live chords, grouped under Pane, Window, Session,
and View. Typing fuzzy-filters the list; Enter runs the selected action.
Navigate with arrows / `C-n` / `C-p` (`j` / `k` when the query is empty),
PageUp / PageDown, Home / End, or the wheel. Enabled plugin `[[actions]]`
and hostable `[[panes]]` appear under Plugin.

<!-- impl-status: shipped; probe: HostedPlacement::Overlay -->
> **Status: shipped.** Every manifest `placement` opens a real server-side
> Terminal: `split` beside the focused pane, `tab` in a new window,
> `zoomed` as a zoomed split, and `overlay` in a floating box.

An **overlay** pane opens in a titled box centered over the pane area
(80% of each axis). It belongs to no window, so it never changes your
layout and other clients attached to the session do not show it. While
it is open, keys, pastes, and clicks inside the box go to it, and the
panes beneath stop updating until it closes. The prefix still works:
`C-a x` (`kill-pane`) closes the overlay and nothing else, and any other
action closes it first and then runs. A click outside the box also
closes it. Closing kills its process; the overlay also closes by itself
when its process exits
([ADR-0147](../adr/0147-plugin-overlay-panes-float-outside-the-layout.md)).

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

The **name prompts** (new session, rename session or window) and the
copy-mode search line edit like a shell: `C-a`/Home and `C-e`/End jump to
the ends, `C-b`/`C-f` and the arrows move, `C-u` and `C-k` kill to the start
or end, `C-w` kills the previous word, `C-h`/Backspace and `C-d`/Delete
delete a character. Enter commits; Esc cancels.

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

The page labels each setting with when it takes effect: **now**, **next
attach**, or **next server start**. See [Config and reload](#config-and-reload).

### Bug reports

`report-bug` (`C-a B`, also a palette row and a session-menu row) writes a
local bundle while you are attached, so the report is correlated with the
session, the focused pane, the client and server logs, and a screen dump.
Nothing is uploaded. The bundle lands under the profile state directory
(`reports/<id>/`; see [`../reference/files.md`](../reference/files.md)),
the path is copied to the host clipboard, and a toast names it.

Hand that path to an agent, or from any shell:

```
phux report              # list bundles; latest first
phux report show         # print the newest report.md
phux report show ID      # print a specific one
phux report new "note"   # logs-and-version only, when the TUI itself is down
```

## Agent fleet

`C-a A` (`agent-fleet`) opens a filterable overlay of the attached session's
panes under session headers, plus satellite agents grouped by agent name.
Rows show name, kind, state (`●` blocked, `◐` working, `◆` done,
`○` idle or unknown), pending-question highlights, and branch or cwd.
Satellite rows identify the host and open the pane beside the focused one.

Enter focuses the chosen pane. Rows under other sessions on this server
are one-step cross-session focus when that peer's layout is cached;
otherwise a single "switch to this session" row. The Agents list in the
sidebar appends the same satellite agents, host badge included, after
the local rows. Live state comes from `phux.agent/v1` and the mirrored
asked flag. The dashboard is live: while it is open, record changes,
asks, spawns, and layout changes rebuild rows in place without
disturbing the query. `phux agent list` remains the exhaustive
cross-session CLI projection. The session picker stays grouped by host.

## Mouse

Mouse handling is on by default. On attach the client enables button-event
tracking plus SGR coordinates on the *outer* terminal and restores them
on detach, so divider, sidebar, and tab drags work in a plain shell. It also
turns on focus reports, so the focused pane sees the window gain and lose
focus, and a drag whose button goes up outside the window ends cleanly.

| Event | Action |
|---|---|
| Click in a pane | Focus, then forward |
| Press / drag a divider | Resize; release commits the layout |
| Drag the sidebar's separator rule | Resize the strip for this attach; `sidebar.width` in the config is unchanged |
| Wheel in a pane | Inner mouse mode gets the wheel; else primary screen scrolls local scrollback (forwarded if the viewport cannot move); alt screen becomes arrows, or is forwarded if alternate-scroll is off |
| Right-click in a pane | Pane context menu, unless the inner program has mouse tracking |
| Click a status-bar tab | `select-window` |
| Drag a status-bar tab onto another tab | Move the window into that slot; an insertion marker follows the pointer |
| Click a status-bar destination | Open Sessions, Commands, Settings, Help, or Copy |
| Click a sidebar row | The same action the keyboard binding would run |
| Drag a sidebar window row onto another window row | Move the window into that slot; an insertion marker follows the pointer |

Hold **Shift** to bypass application mouse reporting and use the host
terminal's native selection. `mouse = false` in `[defaults]` skips
capture entirely. Per-pane, `set-pane mouse off` (palette toggle) drops
this client's mouse handling while that pane is focused; a click on it
still focuses it, which is the path back in.

Right-click opens a menu for the pane, the window, or the session,
listing the actions that apply. The session menu includes sessions, fleet,
settings, and commands. Each row commits the same action a
keybinding would. An inner program with mouse tracking on keeps every
button, so no menu opens over it; bind `context-menu` for the keyboard
path. A terminal resize closes the menu; other overlays reflow.

## Config and reload

The config file, its layers, and the `phux config` verbs are
[`../CONFIG.md`](../CONFIG.md); there is no `set-option`. Reloads are
explicit and never automatic: the `reload-config` action, a settings-page
edit, or `phux config reload` from any shell. A reload rebuilds keybindings,
theme, status bar, and plugin palette rows atomically; on any error the
previous config stays in effect and a toast names it. `[sidebar]` geometry,
`[experimental]`, and `defaults.mouse` need a reattach. Reattaching does not
reload server-side settings; check each key's timing in Settings.

`[experimental] predictive-echo` is unset by default: prediction is on
for a remote attach that actually leaves the machine, off on the local
socket and on loopback `--quic` / `--ws`. Set `true` or `false` to
override. The overlay is a local paint; it never reaches the wire,
another client, or a recording.

## Hooks

The TUI plays no sounds and posts no desktop notifications; a server-side
hook on `agent-state-changed` is the notifier edge. Hooks are
[`../CONFIG.md`](../CONFIG.md) and
[`../reference/hooks.md`](../reference/hooks.md).

## Where to go next

| You want | Read |
|---|---|
| Install and the first attach | [Quickstart](../QUICKSTART.md) |
| Every config key | [Configuration](../CONFIG.md) |
| Every action the dispatcher handles | [Action catalog](../reference/actions.md) |
| Every status-bar widget | [Widgets](../reference/widgets.md) |
| Headless verbs, JSON, `%name` | [Agents](./agents.md) |
| Record a pane or attached session | [Recording](./recording.md) |
| The native macOS client | [Cockpit](./cockpit.md) |
| What is shipped versus a gap | [Concepts](../CONCEPTS.md) |
