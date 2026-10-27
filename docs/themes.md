# Themes

A theme sets the colours of phux's own chrome: the status bar, the
sidebar, pane borders and titles, pickers, and popups. Programs in your
panes keep your terminal's colours. phux themes use the `colors.toml`
format of [Omarchy](https://omarchy.org), the Arch Linux desktop setup,
so any Omarchy theme works as it is.

```sh
phux theme install https://github.com/someone/omarchy-nord-theme
phux theme set nord
```

`phux theme set` writes the choice into your config file and repaints
every attached client.

## Installing a theme

`phux theme install` takes a git URL or a local directory. The directory
must hold `colors.toml` at its root. Nothing in a theme is ever run:

```sh
$ phux theme install https://github.com/someone/omarchy-nord-theme
installed nord (Dark) -> `phux theme set nord`
```

The theme's name comes from the URL the way Omarchy names it: the last
path segment without `.git`, an `omarchy-` or `phux-` prefix, or a
`-theme` suffix. Installing the same name again replaces it. Themes are
kept in `$XDG_DATA_HOME/phux/themes`, or `~/.local/share/phux/themes`.

Plugins can ship themes too. An enabled plugin's themes appear in the
list beside the ones you installed; an installed theme wins over a plugin
theme with the same name.

```text
$ phux theme list
* nord
  phux-dark (plugin com.phux.demo.theme-pack)
  tokyo-night
```

`*` marks the selected theme. `phux theme remove NAME` deletes an
installed theme; to drop a plugin's theme, disable the plugin.

## Selecting a theme

Two keys in `[theme]` select a whole palette. Set one of them:

```toml
[theme]
name = "nord"                     # an installed or plugin theme
# file = "~/.local/state/omarchy/current/theme/colors.toml"
```

`file` follows any `colors.toml` on disk. Pointing it at Omarchy's current
theme, as above, makes phux follow the Omarchy theme switcher; run
`phux config reload` after switching to repaint.
[`phux omarchy setup`](./omarchy.md) sets this up for you, along with a hook
that repaints phux on every theme switch.

`phux theme set NAME` and `phux theme set --file PATH` write these keys
for you, remove the other one so exactly one selects, and signal a reload.
Without a running server, attached clients pick it up the next time you
attach.

`phux theme show` prints the selected theme's resolved colours, or a named
theme's with `phux theme show NAME`, along with the foreground on
background contrast ratio. It flags a ratio below 4.5:1, the WCAG AA
minimum for body text.

## Writing a theme

A theme is a directory with a `colors.toml`. Each key is a colour as
`#rrggbb`:

```toml
mode = "dark"

accent = "#bef264"
selection = "#293628"
muted = "#7c8696"

background = "#0f1218"
lighter_background = "#171b23"

foreground = "#c9d1dc"
dark_foreground = "#9aa4b2"
bright_foreground = "#f4f7fb"

red = "#f87171"
yellow = "#fde047"
green = "#86efac"
cyan = "#67e8f9"
blue = "#7aa2f7"
magenta = "#c4b5fd"
```

You only need the base colours. phux fills in the rest the way Omarchy
does: the `bright_*` colours and the darker backgrounds are derived when
you omit them, legacy keys like `bg`, `fg`, and `color0` through
`color15` are understood, and a theme with no `accent` uses its `blue`.
`mode` is `dark` or `light`; without it, a `light.mode` file beside
`colors.toml` or a light background makes the theme light.

A theme that leaves out a colour phux can't derive fails to load, and the
error names the missing key:

```text
phux: error: theme /path/to/colors.toml: missing `red` (and nothing to derive it from)
```

### How theme colours map onto the chrome

| Theme colour | Paints |
| --- | --- |
| `accent` | Titles, the active tab, the focused pane's border and label, finished agents |
| `foreground` | Recessive text: hints, section headers, idle agents, unfocused pane labels |
| `bright_foreground` | Body text on panels |
| `muted` | Rules, borders, unfocused pane dividers |
| `selection` | The selected row's background |
| `green`, `yellow`, `red` | Key chords and working agents; agents that need you; errors |
| `lighter_background` (dark) or `dark_background` (light) | The sidebar, status bar, and popup panels |

## Overriding one colour

Every other key in `[theme]` overrides one colour **slot** of the chrome,
on top of the selected theme or the shipped palette. Slots set here
always win:

```toml
[theme]
name = "nord"
accent = "#ff9e64"
surface = "reset"
```

A colour is a name (`"cyan"`), hex (`"#7aa2f7"`), an ANSI palette index
(`"12"`), or `"reset"` for your terminal's own default. `phux config init`
writes every slot with its shipped value, commented out.

| Slot | Paints |
| --- | --- |
| `accent` | Modal titles, the query caret, the active tab and focused frame |
| `title` | Section and tab titles |
| `chord` | Key chords in help and the which-key popup |
| `action` | Action labels; `reset` uses your terminal's foreground |
| `dim` | Recessive text: hints and sub-lines |
| `border` | Rules and modal borders |
| `section_header` | Headings inside help and pickers |
| `error` | Error and alarm text |
| `text` | Body text on a filled panel |
| `surface` | The sidebar, status bar, and modal fill; `reset` is transparent |
| `shadow` | The drop shadow under floating modals; `reset` turns it off |
| `selection_fg` | Text of the selected row and the copy-mode strip |
| `selection_bg` | Background of the selected row and the copy-mode strip |
| `attention` | "Needs you": the asked marker and blocked agents |
| `sidebar_section` | The sidebar's section headers |
| `agent_idle` | An agent with nothing that needs you |
| `agent_working` | An agent making progress |
| `agent_blocked` | An agent waiting on you |
| `agent_done` | An agent that finished |
| `divider` | Pane borders away from the focused pane |
| `divider_focus` | The focused pane's border (drawn bold) |
| `pane_title` | An unfocused pane's label on its top border |
| `pane_title_focus` | The focused pane's label (drawn bold) |

Status-bar widgets can use the same colours by name: `fg = "theme.accent"`
in a widget style follows the slot. See
[Status bar and sidebar](./status-bar.md#styles-and-colours).

**Keep the contrast.** Every shipped slot that paints text or a rule
clears 4.5:1 against `surface`, in three steps: rules, then recessive
text, then the accent plus bold for focus. Retinting one slot is fine.
Dropping a step below that floor makes chrome some people can't see at
all.

## Debugging

- `phux theme list` shows what is installed and which theme is selected.
- `phux theme show` prints the resolved colours and the contrast ratio.
- `phux config show` shows what `[theme]` holds after layering.
- `phux config check` flags a slot name phux doesn't know (with the
  nearest real slot) and a colour it can't parse.

```text
$ phux config check
theme.acent: unknown name: unknown theme slot `acent` (did you mean `accent`?); the override is ignored
theme.dim: bad value: `lime` is not a colour (use a name like `cyan`, an index 0-255, or `#rrggbb`); the slot keeps its default
2 problems
```

phux itself ignores either mistake and the slot keeps its colour, so run
the check after editing `[theme]`. A `[theme] name` that matches no
installed theme makes `phux config reload` refuse the file.
