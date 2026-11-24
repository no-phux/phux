---
group: customize
order: 6
---

# Distributions

How to pick, switch, and write a distro: a portable config layer of
defaults, plugins, and optionally agent instructions that sits under your
own `config.toml`, so switching never overwrites your settings.

## Choose and switch

```sh
phux config distro list
phux config distro use crew        # a bundled name, a layer file, or a distro directory
phux config distro current
phux config distro off             # removes only the managed selection
phux config reload
```

`use` validates the new stack before writing and keeps your comments,
settings, and other `extends` entries. It only edits files; the usual
[timing](./config.md#when-a-change-takes-effect) applies, and a running
agent conversation keeps the instructions it loaded.

Names are searched in `$PHUX_DISTROS_DIR`, then `$XDG_DATA_HOME/phux/distros`
(or `~/.local/share/phux/distros`), then a dev binary's source checkout.
`starter` and `crew` ship inside every release and are written under
`~/.local/share/phux/` on first use, rewritten when the phux version
changes. Delete a directory's `.phux-bundled` file to keep your own edits.

| Distro | Adds |
| --- | --- |
| `starter` | Workspace autosave and restore, agent launch integrations, plan-usage hooks |
| `crew` | Starter plus a lead-agent home, skills, a fleet board, and palette entries; `phux launch crew` opens a lead ([guide](../distros/crew/README.md)) |
| `herdr` | An alias for `starter` kept for old configs; unrelated to the [Herdr](https://herdr.dev) project |

## Write a distro

A distro is an ordinary [config layer](./config.md#layers) at
`distros/NAME/NAME.toml`:

```toml
extends = ["../starter/starter.toml"]

[theme]
accent = "#a6f750"

[[plugins-append]]
manifest = "phux-plugin.toml"
```

Paths resolve against the layer's directory. Use `plugins-append` so you
add to other layers' plugins rather than replace them. Check it with
`phux config check PATH` and `phux plugin validate PATH`.

A distro is trusted local code running as you. An agent distro may add
instructions and skills, but must not overwrite global harness settings,
change permission policy, copy credentials, or install tools.

## Omarchy

[Omarchy](https://omarchy.org) is an Arch Linux and Hyprland desktop.
`phux omarchy setup` (safe to rerun; `--dry-run` previews) fits phux into it:

- Selects an `omarchy` distro layer with Omarchy's tmux keys (prefix
  <kbd>Ctrl</kbd> <kbd>Space</kbd>); a distro you had stays underneath.
- Follows Omarchy's current theme unless your config already picks one, and
  adds a `theme-set` hook that runs `phux config reload` on every switch.
- Rebinds <kbd>Super</kbd> <kbd>Ctrl</kbd> <kbd>Return</kbd> (open phux) and
  <kbd>Super</kbd> <kbd>Ctrl</kbd> <kbd>K</kbd> (list its keys), marks Rune's
  window as a terminal, and starts the phux server with the desktop session,
  so panes reach the Wayland clipboard even when you first attach over SSH.
- Adds Install › AI › Phux Rune and Learn › Phux to Omarchy's menu
  (Omarchy 4 and later).

Hyprland and menu edits go in marked blocks that name the phux binary by
full path; nothing outside them changes. If you move the binary, run setup
again. `phux omarchy status` shows which parts are in place, and
`phux omarchy keys` prints every key in effect.

The layer is rewritten on every setup, so put changes in your own
`config.toml`, which wins:

```toml
[keybindings.prefix-table]
"q" = "next-attention"

[theme]
surface = "#1a1b26"    # an opaque bar and sidebar instead of the terminal background
```

To follow Omarchy's theme after picking another:
`phux theme set --file ~/.local/state/omarchy/current/theme/colors.toml`.
Rune follows Omarchy's theme on its own.

`phux omarchy remove` restores your previous distro and theme selection and
deletes the hook, the layer, the marked blocks, and the menu rows. Rune stays
installed.
