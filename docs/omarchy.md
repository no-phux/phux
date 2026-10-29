# Omarchy

[Omarchy](https://omarchy.org) is an Arch Linux and Hyprland desktop setup.
One command puts phux in the places Omarchy keeps a terminal multiplexer:

```sh
phux omarchy setup
```

Then:

| Press | Does |
| --- | --- |
| <kbd>Super</kbd> <kbd>Ctrl</kbd> <kbd>Return</kbd> | Opens a terminal attached to phux |
| <kbd>Super</kbd> <kbd>Ctrl</kbd> <kbd>K</kbd> | Lists phux's keys in Omarchy's menu |

Inside phux, the keys are the ones Omarchy's tmux config uses, so your
hands already know them. Switch Omarchy's theme and phux repaints to match.

## What setup changes

`phux omarchy setup` is safe to run again. It prints every file it writes,
and `--dry-run` prints them without writing anything.

| Change | Where |
| --- | --- |
| A [distro](./distros.md) layer with Omarchy's tmux keys, selected in your config. A distro you already had selected stays underneath it. | `~/.local/share/phux/distros/omarchy/omarchy.toml` |
| phux follows Omarchy's current theme, unless your config already selects a theme. | `[theme] file` in your config |
| A hook that repaints phux after each Omarchy theme change. | `~/.config/omarchy/hooks/theme-set.d/phux` |
| <kbd>Super</kbd> <kbd>Ctrl</kbd> <kbd>Return</kbd> and <kbd>Super</kbd> <kbd>Ctrl</kbd> <kbd>K</kbd> open phux and its key list. | A marked block in `~/.config/hypr/bindings.lua` |
| The phux server starts with your desktop session, so its panes can reach the Wayland clipboard even when you first attach over SSH. | A marked block in `~/.config/hypr/autostart.lua` |

Omarchy gives <kbd>Super</kbd> <kbd>Ctrl</kbd> <kbd>Return</kbd> and
<kbd>Super</kbd> <kbd>Ctrl</kbd> <kbd>K</kbd> to Herdr out of the box;
setup rebinds them to phux. Omarchy's tmux keys,
<kbd>Super</kbd> <kbd>Alt</kbd> <kbd>Return</kbd> and
<kbd>Super</kbd> <kbd>Alt</kbd> <kbd>K</kbd>, are left alone.

setup never edits text outside its marked blocks:

```lua
-- >>> phux: managed by `phux omarchy setup`; `phux omarchy remove` deletes it
o.rebind("SUPER + CTRL + RETURN", "Phux", "omarchy-launch-terminal /home/you/.local/bin/phux")
o.rebind("SUPER + CTRL + K", "Phux keybindings", [[sh -c '/home/you/.local/bin/phux omarchy keys | omarchy-menu-select "Phux keybindings" -- --width 800']])
-- <<< phux
```

The blocks name the phux binary by its full path, because Hyprland's
`PATH` may not include `~/.local/bin`. If you move phux, for example from
the install script to a package, run setup again. An Omarchy that still
uses `hyprland.conf` gets the same blocks in `bindings.conf` and
`autostart.conf`.

Check what's in place:

```text
$ phux omarchy status
✓ omarchy
✓ distro
✓ theme
✓ theme_hook
✓ desktop_keys
✓ autostart
```

## Keys

The prefix is <kbd>Ctrl</kbd> <kbd>Space</kbd>, as in Omarchy's tmux. A
tmux window is a phux tab, and a tmux session is a phux session.

| Key | Does |
| --- | --- |
| <kbd>Alt</kbd> <kbd>Enter</kbd> | Split below |
| <kbd>Alt</kbd> <kbd>Shift</kbd> <kbd>Enter</kbd> | Split right |
| <kbd>Alt</kbd> <kbd>Esc</kbd> | Kill pane |
| <kbd>Ctrl</kbd> <kbd>Alt</kbd> <kbd>←↓↑→</kbd> | Move between panes |
| <kbd>Ctrl</kbd> <kbd>Alt</kbd> <kbd>Shift</kbd> <kbd>←↓↑→</kbd> | Resize the pane |
| <kbd>Alt</kbd> <kbd>1</kbd> … <kbd>9</kbd> | Go to tab 1 to 9 |
| <kbd>Alt</kbd> <kbd>←</kbd> / <kbd>→</kbd> | Previous / next tab |
| <kbd>Alt</kbd> <kbd>Shift</kbd> <kbd>←</kbd> / <kbd>→</kbd> | Move the tab left / right |
| <kbd>Alt</kbd> <kbd>↑</kbd> / <kbd>↓</kbd> | Previous / next session |
| prefix <kbd>h</kbd> / <kbd>v</kbd> | Split below / right |
| prefix <kbd>c</kbd> / <kbd>r</kbd> / <kbd>k</kbd> | New / rename / kill tab |
| prefix <kbd>C</kbd> / <kbd>R</kbd> / <kbd>K</kbd> | New / rename / kill session |
| prefix <kbd>P</kbd> / <kbd>N</kbd> | Previous / next session |
| prefix <kbd>q</kbd> | Reload the config |
| prefix <kbd>?</kbd> | Every command, searchable |

Everything else keeps phux's [default keys](./reference/keys.md). Three
moved to make room for tmux's: prefix <kbd>a</kbd> jumps to the next pane
waiting for you (phux's prefix <kbd>q</kbd>), resizing uses
<kbd>Ctrl</kbd> <kbd>Alt</kbd> <kbd>Shift</kbd> and the arrows instead of
prefix <kbd>R</kbd>, and <kbd>Ctrl</kbd> <kbd>Alt</kbd> and the arrows move
between panes instead of prefix <kbd>h</kbd> and <kbd>k</kbd>.

`phux omarchy keys` prints every key in effect, your own bindings
included, one per line. That is the list <kbd>Super</kbd> <kbd>Ctrl</kbd>
<kbd>K</kbd> shows.

## Your settings still win

The layer sits under your own `config.toml`, so any key you set there
wins. To change one of the keys above, bind it in your config:

```toml
[keybindings.prefix-table]
"q" = "next-attention"
```

To keep the opaque status bar and sidebar of phux's own theme instead of
letting the terminal's background show through, set the
[theme slot](./themes.md#overriding-one-colour):

```toml
[theme]
surface = "#1a1b26"
```

Setup rewrites the layer each time it runs. Put changes in your config,
not the layer.

## Themes

phux reads Omarchy's `colors.toml` as it is, so its sidebar, status bar,
pane borders and popups use the theme's colours. When you switch themes,
Omarchy runs the `theme-set` hook, which runs `phux config reload`, and
attached clients repaint without detaching. Programs inside your panes use
your terminal's colours, which Omarchy changes itself.

If your config already selected a theme before setup, setup keeps it and
tells you. To follow Omarchy's theme afterwards:

```sh
phux theme set --file ~/.local/state/omarchy/current/theme/colors.toml
```

## Undo it

```sh
phux omarchy remove
```

This restores the distro you had selected before setup, stops following
Omarchy's theme if setup turned that on, and deletes the hook, the layer
and the marked Hyprland blocks. Everything else in those files is left as
it was. Omarchy's own bindings for Herdr come back when Hyprland reloads.
