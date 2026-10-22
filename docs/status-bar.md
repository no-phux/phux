# Status bar and sidebar

The status bar is the one row phux keeps for tabs, the session name, and
the clock. The sidebar is the strip down one side that lists your agents
and sessions. Both are built from your [configuration file](./config.md),
and both change on `phux config reload` except where noted.

```toml
[status]
position = "bottom"
right = [
  { kind = "cwd", truncate = 30 },
  { kind = "time", format = " %H:%M " },
]

[sidebar]
width = 28
position = "right"
```

## The status bar

`[status]` has three slots, `left`, `center`, and `right`, each a list of
**widgets** drawn in order. The shipped bar has the tabs and a `+` button
on the left, nothing in the center, and the session name, clock, and a
`+ session` button on the right.

A widget is a table with a `kind` and that kind's options. A bare string
is shorthand for a widget with no options:

```toml
[status]
left = ["windows"]
center = [{ kind = "text", value = "prod" }]
right = ["session-name", { kind = "time", format = "%a %H:%M" }]
```

Setting a slot replaces the shipped list. To keep it and add to the end,
use `-append`:

```toml
[status]
right-append = [{ kind = "exec", command = "uptime | cut -d, -f1", interval = "30s" }]
```

Set all three slots to `[]` and the bar disappears, giving its row back
to the panes. `position` puts the bar on the `"top"` (the default) or
`"bottom"` row.

### Widget kinds

| Kind | Shows |
| --- | --- |
| [`windows`](./reference/widgets.md#windows) | One clickable tab per tab, with agent and zoom markers |
| [`session-name`](./reference/widgets.md#session-name) | The current session's name |
| [`time`](./reference/widgets.md#time) | The clock, in strftime format |
| [`cwd`](./reference/widgets.md#cwd) | The focused pane's working directory |
| [`exit`](./reference/widgets.md#exit) | The focused pane's last exit code (needs shell integration) |
| [`exec`](./reference/widgets.md#exec) | The first line a command prints, rerun on an interval |
| [`text`](./reference/widgets.md#text) | Fixed text |
| [`spacer`](./reference/widgets.md#spacer) | Blank space that pushes its neighbours apart |
| [`button`](./reference/widgets.md#button) | A clickable label that runs an action |
| [`switch`](./reference/widgets.md#switch) | A clickable chip that opens the agents list |
| [`help-hints`](./reference/widgets.md#help-hints) | Clickable hints for the main keys |

[Status-bar widgets](./reference/widgets.md) lists every option of every
kind with its default.

### Styles and colours

Every widget takes a `style` table. `fg` and `bg` are colours; `bold`,
`dim`, `italic`, `underline`, and `reverse` are true or false:

```toml
[status]
center = [{ kind = "text", value = " prod ", style = { fg = "black", bg = "red", bold = true } }]
```

A colour is a name (`"red"`), hex (`"#f87171"`), an ANSI palette index
(`"9"`), or `theme.<slot>` to follow a [theme](./themes.md) slot. The
shipped bar uses theme slots throughout, so a theme recolours it with the
rest of the chrome:

```toml
[status]
left = [
  { kind = "windows", format = " {index} {name} ", active = { fg = "theme.accent", bg = "theme.selection_bg", bold = true }, inactive = { fg = "theme.dim" }, index = { fg = "theme.border" } },
]
```

The `windows` widget styles its parts separately: `active` for the
current tab, `inactive` for the rest, and `index` for the number alone.
`format` is the template for each tab (`{index}` and `{name}`), and
`separator` goes between tabs. Every cell of a tab, padding included, is a
click target.

### Changing shape with the terminal

Every widget takes `min-cols` and `max-cols`. They compare against the
width of the whole bar, and a widget outside its range takes no room at
all, so the rest get its columns:

```toml
[status]
right = [
  { kind = "session-name", min-cols = 65 },
  { kind = "time", format = " %H:%M ", min-cols = 65 },
  { kind = "switch", max-cols = 64 },
]
```

This is what the shipped bar does: on a terminal 64 columns wide or
narrower, the session name and clock give way to an `agents` chip that
opens the agents list.

### Buttons

A `button` runs one action that takes no arguments when you click it:

```toml
[status]
right-append = [{ kind = "button", label = "settings", action = "settings" }]
```

The shipped `+` after the tabs runs `new-window`, and `+ session` runs
`new-session`. Any action from [Actions](./reference/actions.md) that
needs no arguments works.

Plugins can add widgets too. They append to the end of their slot, after
yours.

## The sidebar

The sidebar shows two panels. **Agents** lists every agent with its
state, under a row of filter chips: `all`, `this` (this session's), and
`need` (blocked, asking, or finished and not yet looked at).
**Sessions** lists your sessions with the current one's tabs nested
under it, and a line per machine when you use more than one. Click an
agent to go to its tab; click a session to switch to it.

Press <kbd>C-a</kbd> <kbd>b</kbd> to hide or show it. That choice lasts
until you detach, across session switches. <kbd>C-a</kbd> <kbd>a</kbd>
steps the Agents filter, and phux remembers the filter across attaches.

```toml
[sidebar]
enabled = true        # shown when you attach
width = 20            # columns; 0 adapts to a quarter of the terminal, 28 to 40
position = "left"     # or "right"
agents-share = 50     # percent of the height for Agents; 0 drops the panel
hosts = true          # group sessions by machine
```

Agents and Sessions split the height at a fixed line, so gaining or
losing an agent never moves the Sessions panel. Plugins can add fixed
sections between them.

**`[sidebar]` settings apply when you attach.** `phux config reload`
does not move or resize a sidebar that is already showing; detach and
run `phux attach` again.

### Narrow terminals

The sidebar steps aside when the terminal is too narrow to afford it.
Below its width plus `min-pane-cols` (40 columns by default; see
[Breakpoints](#breakpoints)), phux gives the panes the whole width and
<kbd>C-a</kbd> <kbd>b</kbd> rings the bell instead of toggling. On a
terminal that narrow, <kbd>C-a</kbd> <kbd>A</kbd> (or the status bar's
`agents` chip) opens the agents list full-screen.

### Machines

With `hosts = true`, the Sessions panel lists the sessions of every
machine you use, each under its own line. phux refreshes the other
machines every `hosts-refresh-secs` seconds (default 10, at least 2) by
running its own `phux ls --all --json`. `hosts-provider` replaces that
command with any argv that prints the same JSON. `hosts = false` lists
only the server you're attached to.

## Breakpoints

`[chrome]` sets the sizes at which phux's layout adapts to a small
terminal:

```toml
[chrome]
compact-cols = 64     # at or below: popups fill the width instead of floating
compact-rows = 18     # at or below: popups fill the height
min-pane-cols = 40    # the sidebar needs this many columns left for panes
```

Raise `compact-cols` for full-width pickers on a roomier terminal; lower
`min-pane-cols` to keep the sidebar on a narrower one. `0` turns a
threshold off.

## Notifications

phux notifies you when an agent becomes blocked or asks for you, and when
an agent that was working finishes:

```toml
[notifications]
mode = "terminal"           # "system", "bell", or "off"
terminal-protocol = "auto"  # or "osc9", "osc777", "osc99"
blocked = true              # an agent needs you
done = true                 # an agent finished
suppress-focused = true     # quiet for the pane you're looking at
sound = false
```

| Mode | Delivers |
| --- | --- |
| `terminal` | A notification escape sequence to your terminal, so it reaches your desktop over SSH too |
| `system` | The desktop notifier of the machine the client runs on (`osascript` on macOS, `notify-send` elsewhere) |
| `bell` | The terminal bell |
| `off` | Nothing |

"Finished" fires once per run, and looking at the pane before it goes out
cancels it. A burst of notifications within a second arrives as one.

`terminal-protocol = "auto"` picks OSC 99 for kitty, OSC 777 for Ghostty,
WezTerm, and foot, and OSC 9 for the rest. Over SSH your terminal may not
be identifiable; set the protocol yourself if notifications don't appear.
Inside tmux, phux wraps the sequence for passthrough, which needs
`set -g allow-passthrough on` in tmux.

## Debugging

- `phux config check` reports an unknown widget kind or option with a
  did-you-mean, and a `min-cols` above `max-cols`, as `status.right[0]`
  and so on.
- `phux config show` prints the `[status]` and `[sidebar]` in effect.
- A bar that fails to build shows the error in its place when you attach,
  and `phux config reload` refuses the file.

If a `[sidebar]` change seems ignored, detach and attach again. If the
sidebar won't show at all, the terminal may be narrower than its width
plus `min-pane-cols`.
