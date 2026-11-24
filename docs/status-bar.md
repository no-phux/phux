---
group: customize
order: 3
---

# Status bar and sidebar

How the status bar and sidebar are composed and configured. Every option and
default is in the [config reference](./reference/config.md#the-annotated-default-config);
every widget option is in [Status bar widgets](./reference/widgets.md).

## The status bar

`[status]` has three slots, `left`, `center`, and `right`, each a list of
widgets drawn in order. A widget is a table with a `kind` and that kind's
options; a bare string is a widget with none. `position` is `"top"` or
`"bottom"`.

```toml
[status]
center = [{ kind = "text", value = " prod ", style = { fg = "black", bg = "red", bold = true } }]
right-append = [{ kind = "exec", command = "uptime | cut -d, -f1", interval = "30s" }]
```

Setting a slot replaces the shipped list; `<slot>-append` keeps it and adds
to the end. All three slots `[]` removes the bar and gives its row to the
panes. Plugin widgets go after yours.

### Widget kinds

The kinds are `windows`, `session-name`, `time`, `cwd`, `exit`, `exec`,
`text`, `spacer`, `button`, `switch`, and `help-hints`; their options are in
[Status bar widgets](./reference/widgets.md). A `button` runs one
argument-free [action](./reference/actions.md) on click.

Every kind also takes:

- `style`: `fg`, `bg`, and `bold`, `dim`, `italic`, `underline`, `reverse`.
  Cells a widget styles itself (the `windows` widget's `active` and
  `inactive` tabs, `exec`'s own colours) keep their style; only plain cells
  inherit the widget's.
- `min-cols`, `max-cols`: bounds on the terminal's width (not the bar's).
  Outside them the widget takes no room, so the others get its columns.

```toml
[status]
right = [
  { kind = "session-name", min-cols = 65 },
  { kind = "time", format = " %H:%M ", min-cols = 65 },
  { kind = "switch", max-cols = 64 },
]
```

That is what the shipped bar does: at 64 columns or fewer, the session name
and clock give way to an `agents` chip.

### Styles and colours

A colour is a name, `#rrggbb`, an ANSI index, or `theme.<slot>` to follow a
[theme](./themes.md#override-one-slot) slot. The shipped bar uses theme slots
throughout, so a theme recolours it with the rest of the chrome.

## The sidebar

**Agents** lists every agent under filter chips (`all`, `this`, `need`);
**Sessions** lists sessions with the current one's tabs nested, grouped by
machine. Click a row to go there. <kbd>C-a</kbd> <kbd>b</kbd> toggles it for
the rest of the attach; <kbd>C-a</kbd> <kbd>a</kbd> steps the filter, which
is remembered across attaches. Plugins can add fixed sections between the
two panels.

```toml
[sidebar]
width = 0           # 0 adapts to a quarter of the terminal, 28 to 40 columns
position = "right"
agents-share = 50   # percent of the height for Agents; 0 drops the panel
```

**`[sidebar]` applies on attach**, not on reload: detach and attach again.

Below the sidebar's width plus `[chrome] min-pane-cols`, the sidebar steps
aside and <kbd>C-a</kbd> <kbd>b</kbd> rings the bell; <kbd>C-a</kbd>
<kbd>A</kbd> or the `agents` chip opens the agents list full-screen instead.

With `hosts = true`, Sessions lists every machine you use, refreshed every
`hosts-refresh-secs` by running `phux ls --all --json`. `hosts-provider`
replaces that command with any argv that prints the same JSON.

## Breakpoints

`[chrome]` `compact-cols` and `compact-rows` are the sizes at or below which
popups fill the width or height instead of floating; `min-pane-cols` is the
pane width the sidebar must leave. `0` turns a threshold off.

## Notifications

`[notifications]` notifies when an agent becomes blocked or asks for you,
and once when a working agent finishes (cancelled if you look first). `mode`
is `terminal` (an OSC sequence, so it reaches your desktop over SSH),
`system`, `bell`, or `off`. `terminal-protocol = "auto"` guesses the OSC
dialect from the terminal; over SSH it may guess wrong, so pin `osc9`,
`osc777`, or `osc99`. Inside tmux it needs `set -g allow-passthrough on`.

A `warn` or `error` [note](./scripting.md#notes) for a pane you can't see
notifies the same way.

### Muting a pane

```sh
phux notify-mode @4 off              # this pane
phux notify-mode work unread-only    # every pane in session "work" now
phux notify-mode @4 on
```

`unread-only` keeps state, badges, and counts but raises no toast, sound, or
system notification; `off` also drops the pane from counts such as the Dock's.
The server stores the mode in the pane's `phux.notify/v1` metadata, so every
client honours it and it survives `phux upgrade`. A session, group, or tag
target sets the panes it holds now; panes added later start `on`.

## Debugging

`phux config check` reports an unknown widget kind or option (with a
suggestion) and a `min-cols` above `max-cols`, at paths like
`status.right[0]`. A bar that fails to build shows the error in its place,
and `phux config reload` refuses the file.
