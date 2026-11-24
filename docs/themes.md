---
group: customize
order: 2
---

# Themes

How to install, select, write, and override colour themes for phux's own
chrome: the status bar, sidebar, pane borders, pickers, and popups. Programs
in your panes keep your terminal's colours.

```sh
phux theme install https://github.com/someone/omarchy-nord-theme
phux theme set nord
```

## Install and select

A theme is a directory with an [Omarchy](https://omarchy.org) `colors.toml`
at its root, so any Omarchy theme works unchanged. `phux theme install`
takes a git URL, a directory, or a Ghostty theme file (converted on install).
Nothing in a theme runs. Enabled plugins can ship themes too; an installed
theme wins over a plugin theme of the same name.

`[theme]` selects a palette with exactly one of two keys:

```toml
[theme]
name = "nord"                                                     # installed or plugin theme
# file = "~/.local/state/omarchy/current/theme/colors.toml"      # any colors.toml on disk
```

`phux theme set NAME` or `phux theme set --file PATH` writes the key, removes
the other, and signals a reload. Pointing `file` at Omarchy's current theme
follows its switcher; [`phux omarchy setup`](./distros.md#omarchy) wires the
repaint hook for you. With nothing selected, the chrome uses the house look
(`phux`, or `phux-light` in light mode).

`phux theme list` marks the selected theme with `*`; `phux theme show [NAME]`
prints resolved colours and the foreground-on-background contrast, flagging
anything under 4.5:1.

## Write a theme

Each key in `colors.toml` is `#rrggbb`. The base colours are enough:

```toml
mode = "dark"
accent = "#bef264"
selection = "#293628"
background = "#0f1218"
foreground = "#c9d1dc"
red = "#f87171"
yellow = "#fde047"
green = "#86efac"
cyan = "#67e8f9"
blue = "#7aa2f7"
magenta = "#c4b5fd"
```

phux fills the rest the way Omarchy does: `bright_*` and darker backgrounds
are derived, legacy `bg`, `fg`, and `color0`..`color15` are read, and a theme
without `accent` uses `blue`. Without `mode`, a `light.mode` file beside
`colors.toml` or a light background makes it light. A colour that cannot be
derived fails the load and the error names it.

## How a theme is resolved

phux never paints a theme's colours directly. The
[design-language solver](./design-language.md)
turns the palette into roles: `background` is the ground, `foreground` the
body text and its dim and faint tiers, `accent` the focus colour,
`selection` the selected row, and `yellow`, `blue`, `green`, `red` the
needs-you, working, done, and failed tones. Each colour moves toward black
or white only as far as it must to clear 4.5:1 on every panel; a theme that
already clears it comes through unchanged.

The result is then fitted to the terminal:

- 24-bit (`COLORTERM=truecolor` or `24bit`): as solved.
- 256 colours: snapped to entries 16 to 255 (never 0 to 15, which you may
  have remapped), stepping further if snapping costs contrast.
- 16 colours: your terminal's ANSI colours, selection in reverse video.
- `NO_COLOR`: no colour. Every state also has a mark and a word, so nothing
  is lost.

At attach phux asks the terminal for its background (OSC 11) and solves
against it; with no theme selected the reply also picks light or dark.
Without a reply, `COLORFGBG` picks the mode, else dark.

## Override one slot

Every other `[theme]` key overrides one chrome slot on top of the selected
theme, and always wins. The slot names and shipped values are in the
`[theme]` block of the [annotated defaults](./reference/config.md#the-annotated-default-config).

```toml
[theme]
name = "nord"
accent = "#ff9e64"
surface = "reset"        # transparent sidebar, status bar, and modal fill
```

A colour is a name (`"cyan"`), hex, an ANSI index (`"12"`), or `"reset"` for
the terminal default. A slot you set is used exactly as written on every
terminal, never snapped, so on a terminal without 24-bit colour use an index
or name, and keep it above 4.5:1 against `surface`. Status-bar widgets can
name a slot as `theme.<slot>` ([Styles](./status-bar.md#styles-and-colours)).

## Debugging

`phux config check` flags an unknown slot (with the nearest name), an
unparsable colour, and a `name` or `file` that loads no theme. phux itself
ignores a bad slot and keeps its colour, but a `name` that matches no theme
makes `phux config reload` refuse the file.

```text
theme.acent: unknown name: unknown theme slot `acent` (did you mean `accent`?); the override is ignored
```
