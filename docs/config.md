# Configuration

phux reads one file, `~/.config/phux/config.toml` (or
`$XDG_CONFIG_HOME/phux/config.toml`). It is optional. Whatever you put in it
overrides the built-in defaults key by key, so keep it to the settings you
actually change.

```sh
phux config path            # where the file is
phux config init            # write a starter file with every setting commented out
phux config show --default  # print the built-in defaults
phux config show            # print what is in effect
phux config check           # validate the file
phux config reload          # make attached clients pick up changes
```

The file is not watched; run `phux config reload` after editing it. Every
section and setting is listed in the [config reference](./reference/config.md).

## Keys

```toml
[keybindings]
prefix = "C-b"

[keybindings.prefix-table]
"v" = { action = "split-pane", direction = "vertical" }
"r" = "reload-config"

[keybindings.global]
"M-Enter" = "toggle-zoom"
```

`prefix-table` bindings fire after the prefix; `global` bindings fire
without it. Chords are written `C-x` (Ctrl), `M-x` (Alt), `S-x` (Shift), and
combinations like `C-M-x`; named keys include `Enter`, `Tab`, `Space`,
`Escape`, the arrows, and `F1` to `F24`. A binding that does not parse is
skipped with a warning; the rest still load.

Every action you can bind, with its parameters, is in
[actions](./reference/actions.md). The command palette (`Ctrl-A Space`)
shows the same list with the current keys.

## Themes

A theme is a `colors.toml` in the Omarchy format, so Omarchy themes work
as they are.

```sh
phux theme list
phux theme install https://example.com/my-theme.git
phux theme set tokyo-night
```

Individual colors can be overridden in `[theme]`; `phux config init` writes
the full list of color slots, commented out.

## Status bar and sidebar

`[status]` sets what the bar shows, as lists of widgets for the left,
center, and right. `[sidebar]` controls the sidebar (`Ctrl-A b`). The widget
kinds and their options are in [widgets](./reference/widgets.md).

## Hooks

A hook runs a command when the server sees an event:

```toml
[[hooks.pane-exit]]
when   = { exit-code = "*" }
action = { kind = "run", command = "notify-send 'pane exited'" }
```

Events, the fields a `when` clause can match, and the environment the
command gets are in [hooks](./reference/hooks.md).

## Plugins

A plugin is a directory with a manifest that can add actions, status
widgets, sidebar sections, themes, hooks, and agent launch commands. phux
never loads code into the server; a plugin's actions run as separate
processes.

```sh
phux plugin install https://github.com/someone/phux-plugin
phux plugin list
phux plugin disable NAME
```

[`examples/plugins`](../examples/plugins) has working plugins to copy.

## Projects

A repository can carry `.phux/project.toml`, describing the tabs and panes
to open for it. phux runs it only after you approve its exact contents.

```sh
phux project init       # write .phux/project.toml for this repository
phux project trust      # approve it
phux project open       # open the project's session, or attach if it exists
```

Edit the file and you have to `phux project trust` it again.

## Layers

A config file can declare `extends = ["path/to/other.toml"]` to build on
other files. The layers apply in order: built-in defaults, then each
extended file, then yours. A key named `x-append` holding an array appends
to `x` instead of replacing it. `phux config show --layers` shows where each
value came from. `phux config init --distro starter` writes a config that
extends [`distros/starter`](../distros/starter), which enables the example
plugins.
