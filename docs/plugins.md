# Plugins

A plugin is a directory with a `phux-plugin.toml` manifest that adds to
phux: actions for the palette and your keys, panes, status-bar widgets,
sidebar sections, themes, hooks, agent declarations, and long-running
startup sidecars. Install one to pick up someone else's workflow, or
write one to package your own.

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
`[[plugins]]` entries. The `phux plugin` commands edit those entries.
`enable` and `disable` also ring the config-reload doorbell so a running
server starts or stops that plugin's startup sidecars. The other
commands only edit the file:

| Command | Does |
| --- | --- |
| `phux plugin install REF` | Fetches a git URL, local directory, or tarball, runs its build steps, validates it, and links it |
| `phux plugin update [NAME]` | Re-fetches, rebuilds, and revalidates installed plugins, or just NAME |
| `phux plugin link PATH` | Adds a manifest already on disk, such as one you're writing |
| `phux plugin unlink ID` | Removes a plugin's entry |
| `phux plugin enable ID` | Turns a plugin on and, if a server is running, starts its startup sidecars |
| `phux plugin disable ID` | Turns a plugin off, keeps its entry, and stops its startup sidecars |
| `phux plugin list` | Lists configured plugins and whether each is enabled |
| `phux plugin validate [PATH]` | Checks one manifest, or every configured one |
| `phux plugin log` | Shows recent plugin action, hook, and startup lifecycle lines |
| `phux plugin log --startup [ID]` | Prints the tail of a startup sidecar's log |

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
reads them at start; run `phux upgrade` to restart it in place. Startup
sidecars are different: `enable`, `disable`, and `phux config reload`
reconcile them in the running server. An entry whose manifest has not
changed is left running.

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
| `[[startup]]` | A long-running sidecar the server supervises. See [Startup sidecars](#startup-sidecars). |

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

## Startup sidecars

A `[[startup]]` entry is a process the server keeps running for the life
of the server. It is not a hook (hooks are killed after 30 seconds) and
not a pane. One process runs per entry. Use it for a local web server or
anything else that should come up with phux and go down with it.

```toml
[[startup]]
id = "web"
command = ["bun", "server.ts"]
platforms = ["linux", "macos"]
```

`command` is an argument list, never a shell string. Write
`["sh", "-c", "..."]` when you need a shell. `id` is unique in the
manifest and uses the same rules as an action id. A plugin id or a
startup id of `.`, `..`, or `server` is rejected: those names escape
the startup directory or collide with files the server writes there.
`platforms` is optional. The entry runs only when both that list and the manifest's
top-level `platforms` allow this operating system. Leaving a list out
means every system. A manifest may declare at most 8 startup entries.
The server starts at most 8 across every enabled plugin, in `[[plugins]]`
order; the rest are skipped and a `skipped: cap` line is written to the
run log. `phux plugin validate` checks the per-manifest rules on every
operating system. It does not apply the platform filter, so one machine
can validate a multi-platform manifest. There is no `timeout` key.

The server starts sidecars after it is accepting connections, including
after `phux upgrade`. The child runs as you, with cwd set to the plugin
directory (`PHUX_PLUGIN_ROOT`) and umask `077`. Its environment is the
server's, plus:

| Variable | Value |
| --- | --- |
| `PHUX_SOCKET` | This server's socket |
| `PHUX_PLUGIN_ID` | Manifest `id` |
| `PHUX_PLUGIN_NAME` | Manifest `name` |
| `PHUX_PLUGIN_VERSION` | Manifest `version` |
| `PHUX_PLUGIN_ROOT` | Directory containing the manifest |
| `PHUX_PLUGIN_STATE` | `<state-dir>/plugin-startup/<plugin-id>/<startup-id>/`, mode `0700` |

Pane identity the server may have inherited (`PHUX_TERMINAL_ID`,
`PHUX_PANE`, `PHUX_SESSION`, and the other per-pane variables) is
removed. Stdin is `/dev/null`. Stdout and stderr append to
`<state-dir>/plugin-startup/<plugin-id>/<startup-id>.log`, mode `0600`.
The server rotates that file at 1 MiB, keeping three older generations,
by copying it aside and truncating it in place.

A non-zero exit or a signal restarts the process after a backoff of 1,
2, 4, 8, 16, then 30 seconds. The backoff returns to 1 second after the
process has been up for at least 60 seconds. Exit 0 is a deliberate
stop: the run log records `exited 0 (not restarting)` and the process
is not started again. The sixth failure inside ten minutes gives up
until the next server start or a disable/enable cycle.

On server exit, upgrade, and plugin disable, every sidecar's process
group gets `SIGTERM`, then `SIGKILL` after a shared 5 second deadline.
Upgrade stops them before the new binary runs and starts them again
once the new process is accepting; they are not restored from the
upgrade handoff. A server killed with `SIGKILL` cannot do that. The
next start reads the pidfile, which names the sidecar and the server
that owned it. It does not signal the group while that server is still
alive. When the owner is gone, it signals the group if the leader pid
is still alive and its start time still matches, or if the leader is
already dead but the process group still has a member that started in
or after the recorded spawn tick (a grandchild left behind when the
leader honored `SIGTERM`, including one forked in the same 10ms tick).
A live pid that started at a different time is left alone. A pidfile
from a different boot is stale and is not signaled, because start-time
ticks are not comparable across reboots. The pidfile is named
`<startup-id>.<server-pid>.pid` so two servers sharing a state
directory do not overwrite each other. Pidfiles for sidecars that have
already exited are removed.

**A sidecar's socket connection keeps the server alive.** Connecting to
`PHUX_SOCKET` counts as a client connection, the same as any other
client. While that connection is open, `--exit-after-idle` does not
fire. A sidecar that never connects does not keep the server alive by
merely existing.

**The sidecar gets owner-level access to `PHUX_SOCKET`.** It can do
anything you can do through phux. It is not sandboxed. Narrower
per-plugin keys come later. Read a plugin before you install it.

## Debugging

- `phux plugin validate` checks every configured manifest and says what
  is wrong with one that fails, including an event hook that can never
  fire.
- `phux plugin list` shows which plugins are enabled.
- `phux config keys` lists plugin keys with `plugin <id>` as their source.
- `phux plugin log` lists the newest runs of every action and hook, and
  startup lifecycle lines, oldest first, with exit status and duration.
  `--failed` shows only failures, with the end of their output; `-n`
  sets how many runs. `phux plugin log --startup` prints the tail of
  each sidecar log; `--startup plugin-id/startup-id` prints one.

```text
$ phux plugin log --failed
2026-10-21 22:34:06  action  example.pane-tools/summarize  exit 1  79ms
    summarize.py: no pane text
```

If an action is missing from the palette, check that the plugin is
enabled and run `phux config reload`. If its hook never shows up in the
log, the server hasn't restarted since you enabled it.
