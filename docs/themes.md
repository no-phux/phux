# Themes

A theme sets the colours of phux's own chrome: the status bar, the
sidebar, pane borders and titles, pickers, and popups. Programs in your
panes keep your terminal's colours. phux themes use the `colors.toml`
format of [Omarchy](https://omarchy.org), the Arch Linux desktop setup,
so any Omarchy theme works as it is.

With no theme selected, the chrome uses the phux house look, `phux` in
dark mode or `phux-light` in light mode. Every theme, the house look
included, goes through one solver from the
[phux design language](./design-language.md), so chrome text stays
readable on any palette.

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

A Ghostty theme file installs the same way. Give it the file, and phux
converts its palette, background, foreground and selection colours:

```sh
phux theme install "/Applications/Ghostty.app/Contents/Resources/ghostty/themes/Catppuccin Mocha"
phux theme set catppuccin-mocha
```

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

phux does not paint a theme's colours directly. It passes the theme to
the design language's solver, the same one the desktop and phone apps
use, and paints the roles that come out. The solver reads these keys:

| Theme colour | Solver input |
| --- | --- |
| `background` | The ground. The panel fill (`surface`) is one step off it. |
| `foreground` | Body text, and the source of the dim and faint tiers |
| `color0` to `color15` (or `red`, `green`, ...) | The status tones: `yellow` for needs you, `blue` for working, `green` for done, `red` for failed |
| `accent` | The focus colour |
| `selection` | The selected row's background |
| `cursor` | Kept as the cursor colour |

The solver moves each colour toward black or white only as far as it
needs to reach 4.5:1 on every panel. When a theme already clears that, its
colours come through unchanged.

## Status marks

An agent's state is shown by a mark and a colour together, and the mark
is always enough on its own. That keeps it readable without colour,
under `NO_COLOR`, and for colour-blind users.

| Mark | ASCII | State | Slot |
| --- | --- | --- | --- |
| `●` | `*` | Needs you: waiting on a human, or a pane that asked | `agent_blocked`, `attention` |
| `✕` | `x` | Failed: exited nonzero, crashed, or its host was lost | `agent_failed` |
| `◆` | `+` | Done, and you have not looked yet | `agent_done` |
| `◐` | `~` | Working | `agent_working` |
| `○` | `o` | Idle, or done and already seen | `agent_idle`, `agent_done` |
| `·` | `.` | Unknown | `sidebar_section` |
| `–` | `-` | Ended: exited cleanly with no agent | `sidebar_section` |

The status bar shows how many panes need you as `●2`. A window row
marks a pane that needs you with `●`, a failed pane with `✕` and its exit
code, and a clean exit with `–`. Under a Chinese, Japanese or Korean
locale, or a locale that is not UTF-8, the whole ladder switches to the
ASCII column, because those marks can draw two cells wide there.

## Terminal colours

phux fits the chrome to what your terminal can show:

- **24-bit colour** (`COLORTERM=truecolor` or `24bit`) paints the roles
  as they are.
- **256 colours** (`TERM` ending in `256color`) snaps each role to the
  nearest entry from 16 to 255. phux never uses entries 0 to 15, because
  you may have remapped them. If snapping costs a colour its contrast, it
  moves one more step toward white or black.
- **16 colours** paints the chrome in your terminal's own ANSI colours:
  cyan for focus, yellow for needs you, red for failed, green for done,
  and bright blue for working (blue on a light background). Body text
  uses your terminal's foreground, and a selected row is drawn in reverse
  video.
- **`NO_COLOR`** turns colour off. The marks, the words and bold still
  carry every state, and a selected row is drawn in reverse video.

When phux attaches, it asks your terminal for its background colour
(OSC 11) and solves the chrome against that, so text on the terminal's
own background stays readable. With no theme selected, the reply also
picks light or dark mode. If the terminal doesn't answer, `COLORFGBG`
picks the mode, and otherwise phux uses dark. When phux doesn't know the
background, pane borders and titles are drawn on the panel fill, since no
text colour is safe on an unknown background.

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
| `accent` | Focus: modal titles, the query caret, the active tab and focused frame |
| `title` | Section and tab titles (body text unless you set it) |
| `chord` | Key chords in help and the which-key popup, drawn bold |
| `action` | Action labels |
| `dim` | Recessive text: hints and sub-lines |
| `border` | Rules and modal borders |
| `section_header` | Headings inside help and pickers |
| `error` | Error and alarm text |
| `text` | Body text on a filled panel |
| `surface` | The sidebar, status bar, and modal fill; `reset` is transparent |
| `shadow` | The drop shadow under floating modals; `reset` turns it off |
| `selection_fg` | Text of the selected row and the copy-mode strip |
| `selection_bg` | Background of the selected row and the copy-mode strip |
| `attention` | Needs you: the status-bar count and the asked marker |
| `sidebar_section` | The sidebar's section headers, and the unknown and ended marks |
| `agent_idle` | An agent with nothing that needs you |
| `agent_working` | An agent making progress |
| `agent_blocked` | An agent that needs you |
| `agent_done` | An agent that finished |
| `agent_failed` | A pane that failed: a nonzero exit, a crash, or a lost host |
| `divider` | Pane borders away from the focused pane |
| `divider_focus` | The focused pane's border (drawn bold) |
| `pane_title` | An unfocused pane's label on its top border |
| `pane_title_focus` | The focused pane's label (drawn bold) |

Status-bar widgets can use the same colours by name: `fg = "theme.accent"`
in a widget style follows the slot. See
[Status bar and sidebar](./status-bar.md#styles-and-colours).

**Keep the contrast.** Every slot that paints text or a rule clears
4.5:1 against `surface`. The rules use the faint tier, recessive text the
dim tier, and focus the accent plus bold, so the three stay in that
order. A slot you set yourself is used exactly as you wrote it, on any
terminal: phux does not snap it to 256 or 16 colours. On a terminal
without 24-bit colour, write an ANSI index (`"12"`) or a name (`"cyan"`).
Retinting one slot is fine. Dropping a slot below the 4.5:1 floor makes
chrome some people can't see at all.

## Debugging

- `phux theme list` shows what is installed and which theme is selected.
- `phux theme show` prints the resolved colours and the contrast ratio.
- `phux config show` shows what `[theme]` holds after layering.
- `phux config check` flags a slot name phux doesn't know (with the
  nearest real slot), a colour it can't parse, and a `name` or `file`
  that selects no loadable theme.

```text
$ phux config check
theme.acent: unknown name: unknown theme slot `acent` (did you mean `accent`?); the override is ignored
theme.dim: bad value: `lime` is not a colour (use a name like `cyan`, an index 0-255, or `#rrggbb`); the slot keeps its default
2 problems
```

phux itself ignores either slot mistake and the slot keeps its colour, so
run the check after editing `[theme]`. A `[theme] name` that matches no
installed theme makes `phux config reload` refuse the file; the check
reports it first (`theme.name: unknown name: no theme named "nordd"`).
