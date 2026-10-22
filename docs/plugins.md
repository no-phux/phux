# Plugins

A plugin is a directory with a `phux-plugin.toml` manifest that adds to
phux: actions for the palette and your keys, panes, status-bar widgets,
sidebar sections, themes, hooks, and agent declarations. Install one to
pick up someone else's workflow, or write one to package your own.

```sh
phux plugin install https://github.com/someone/phux-plugin
phux config reload
```

`phux config reload` makes the plugin's actions, keys, panes, widgets, and
sidebar sections appear in attached clients.

**A plugin runs as you.** phux never loads plugin code into the server,
and every plugin command runs as a separate process, but that is not a
sandbox. A plugin's commands, and the build steps `phux plugin install`
and `phux plugin update` run, can do anything your user can. Read a
plugin before you install it.

## Managing plugins

Plugins are listed in your [configuration file](./config.md) as
`[[plugins]]` entries. The `phux plugin` commands edit those entries for
you and never touch a running server:

| Command | Does |
| --- | --- |
| `phux plugin install REF` | Fetches a git URL, local directory, or tarball, runs its build steps, validates it, and links it |
| `phux plugin update [NAME]` | Re-fetches, rebuilds, and revalidates installed plugins, or just NAME |
| `phux plugin link PATH` | Adds a manifest already on disk, such as one you're writing |
| `phux plugin unlink ID` | Removes a plugin's entry |
| `phux plugin enable ID` | Turns a plugin on |
| `phux plugin disable ID` | Turns a plugin off and keeps its entry |
| `phux plugin list` | Lists configured plugins and whether each is enabled |
| `phux plugin validate [PATH]` | Checks one manifest, or every configured one |
| `phux plugin log` | Shows recent plugin action and hook runs |

```sh
$ phux plugin list
com.phux.usage 0.1.0 (enabled)
com.phux.demo.theme-pack 0.1.0 (disabled)
```

`install` copies the plugin into `$XDG_DATA_HOME/phux/plugins` (or
`~/.local/share/phux/plugins`) and records where it came from, so
`update` can fetch it again. `--rev` installs a branch or tag, and
`--disabled` links it turned off. A plugin can require a minimum phux
version; installing it on an older phux fails with a message saying so.

`link` takes a manifest path, or a directory holding one. The entry it
writes looks like this, and you can write it by hand:

```toml
[[plugins]]
manifest = "~/code/my-plugin/phux-plugin.toml"
enabled = true
```

A relative `manifest` path is relative to the config file that names it.

**Hooks need a server restart.** Enabling, disabling, or updating a
plugin reaches the palette, keys, panes, widgets, and sidebar on
`phux config reload`. Its `[[events]]` hooks run in the server, which
reads them at start; run `phux upgrade` to restart it in place.

## Writing a plugin

A minimal plugin is a manifest and the scripts it runs:

```toml
# phux-plugin.toml
id = "example.pane-tools"
name = "Pane Tools"
version = "0.1.0"
min_phux_version = "0.0.2"
description = "Summarize the focused pane."

[[actions]]
id = "summarize"
title = "Summarize pane"
contexts = ["pane"]
command = ["python3", "summarize.py"]
keys = "t s"

[[events]]
id = "log-exit"
title = "Log pane exits"
on = "pane-exit"
command = ["sh", "-c", "echo \"$PHUX_EXIT_CODE\" >> exits.log"]
```

Link it while you work on it, check it, then reload:

```sh
phux plugin link ~/code/pane-tools
phux plugin validate ~/code/pane-tools
phux config reload
```

`id` must be unique across your plugins. `command` is always an argument
list, run directly and never through a shell unless you call one, as
above. Commands run from the plugin's own directory, so relative paths
like `summarize.py` work.

### What a manifest can declare

| Table | Adds |
| --- | --- |
| `[[actions]]` | A command in the palette. `keys` binds it in the prefix table (`"t s"` is <kbd>C-a</kbd> <kbd>t</kbd> <kbd>s</kbd>); a key that is already bound, by you or by the shipped defaults, keeps its binding and the plugin's is dropped. |
| `[[events]]` | A hook: `on` names a [hook event](./hooks.md#events), and `command` runs every time it fires, with the same environment as a config hook. An `on` that is no hook event would never fire: `phux plugin validate` rejects it, suggesting the nearest event, and the server skips it with a warning. |
| `[[panes]]` | A program to open as a pane. `placement` is `split`, `tab`, `zoomed`, or `overlay` (a floating box). |
| `[[widgets]]` | A status-bar widget: `id`, a `slot` (`left`, `center`, or `right`, the default), and a `kind` with its [options](./reference/widgets.md). It goes after your own widgets. |
| `[[sidebar]]` | A fixed section between Agents and Sessions: `title`, a `format` for each pane's row, and `rows` (1 to 8, default 3). |
| `[[themes]]` | A [theme](./themes.md): a `name` and the `path` of a directory holding `colors.toml`. |
| `[[agents]]` | An agent the plugin integrates, with a `label`, a declared `state`, and an `attention` level. |
| `[[build]]` | A `command` that `install` and `update` run to build the plugin. |

`platforms = ["linux", "macos"]` limits the plugin, or one entry, to those
systems.

A sidebar section's `format` uses `{agent}`, `{cwd}`, `{exit}`,
`{index}`, `{state}`, `{title}`, and `{window}`. A pane gets a row only
when every token in the format has a value for it:

```toml
[[sidebar]]
id = "exits"
title = "Last exit"
format = "{window}: exit {exit}"
rows = 3
```

### Running actions

An action runs from the command palette, from the key in its `keys`, or
from a shell:

```sh
phux config run example.pane-tools summarize
```

To give a plugin action a key, set `keys` in its manifest, or bind it in
your own `[keybindings]` with the `plugin-action` action, naming the
plugin and the action's `id`:

```toml
[keybindings.prefix-table]
"Y" = { action = "plugin-action", plugin = "example.pane-tools", id = "summarize" }
```

Your bindings win over a manifest's `keys`. Binding the same key yourself
takes it back, and a key you set to `"none"` stays unbound even when a
plugin's `keys` names it.

`phux config run` prints the action's output and exits with its exit
code. `--timeout SECS` gives up after that long, and `--json` prints the
result as a JSON document.

## Examples

The phux repository has working plugins to copy in
[examples/plugins](https://github.com/no-phux/phux/tree/main/examples/plugins):

- `usage` publishes your AI plan limits to every client, from hooks on
  `client-attached` and `agent-state-changed`.
- `continuum` saves and restores your workspace.
- `agent-tools` launches and drives a bench of coding agents.
- `theme-pack` is the smallest plugin there is: one theme.

`phux config init --distro starter` writes a config that turns on
`continuum`, `agent-tools`, and `usage` from a phux source checkout.

## Debugging

- `phux plugin validate` checks every configured manifest and says what
  is wrong with one that fails, including an event hook that can never
  fire.
- `phux plugin list` shows which plugins are enabled.
- `phux config keys` lists plugin keys with `plugin <id>` as their source.
- `phux plugin log` lists the newest runs of every action and hook, oldest
  first, with exit status and duration. `--failed` shows only failures,
  with the end of their output; `-n` sets how many runs.

```text
$ phux plugin log --failed
2026-10-21 22:34:06  action  example.pane-tools/summarize  exit 1  79ms
    summarize.py: no pane text
```

If an action is missing from the palette, check that the plugin is
enabled and run `phux config reload`. If its hook never shows up in the
log, the server hasn't restarted since you enabled it.
