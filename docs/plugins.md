---
group: customize
order: 5
---

# Plugin API: build extensions for the phux terminal multiplexer

phux is a terminal multiplexer with a plugin API. A plugin is a directory
with a `phux-plugin.toml` manifest whose commands run as separate processes.
It adds actions, panes, status-bar widgets,
sidebar sections, themes, hooks, agent declarations, and supervised
sidecars. Every `phux plugin` subcommand and flag is in the
[CLI reference](./reference/cli.md#phux-plugin); the manifest schema is
[plugin.schema.json](./spec/plugin.schema.json).

**A plugin runs as you.** phux never loads plugin code into the server and
runs every command as a separate process, but that is not a sandbox. A
plugin's commands and build steps can do anything your user can. Read one
before you install it.

## Managing plugins

```sh
phux plugin install someone/phux-plugin          # github.com/someone/phux-plugin
phux plugin install someone/dotfiles/plugins/x   # a subdirectory
phux plugin install https://git.example/p.git --subdir plugins/x --rev v1.2.0
phux config reload
```

`install` copies the plugin, runs its `[[build]]` steps, and adds a
`[[plugins]]` entry to your config; sources, `plugins.lock`, and `update`
are in the [CLI reference](./reference/cli.md#phux-plugin-install).

Trust is explicit. A git install or update with `[[build]]` steps prints
them and asks `[y/N]` (`--yes` skips; without a terminal it refuses).
Approval is remembered by a hash of the step argv, not of the files those
steps run. A token in a URL is stripped before it is stored or printed; use
a git credential helper for private repositories.

When changes apply:

- Actions, keys, panes, widgets, sidebar sections, themes: `phux config reload`.
- `[[events]]` hooks: server start (`phux upgrade` restarts in place).
- `[[startup]]` sidecars: `phux plugin enable`, `disable`, and
  `phux config reload` reconcile them in the running server.

## Writing a plugin

```sh
phux plugin new hello        # manifest, one action, one hook, a script, a README
phux plugin validate hello
phux plugin link hello
```

A manifest:

```toml
id = "example.pane-tools"
name = "Pane Tools"
version = "0.1.0"
min_phux_version = "0.0.2"

[[actions]]
id = "summarize"
title = "Summarize pane"
contexts = ["pane"]
command = ["python3", "summarize.py"]
keys = "t s"                       # C-a t s; your bindings win

[[events]]
id = "log-exit"
on = "pane-exit"                   # a hook event
command = ["sh", "-c", "echo \"$PHUX_EXIT_CODE\" >> exits.log"]

[[sidebar]]
id = "exits"
title = "Last exit"
format = "{window}: exit {exit}"   # a pane gets a row only when every token resolves
rows = 3
```

`command` is always an argv, never a shell string; it runs from the plugin
directory. The tables are `[[actions]]`, `[[events]]`, `[[panes]]`
(`placement` = `split`, `tab`, `zoomed`, `overlay`), `[[widgets]]` (a
[widget](./reference/widgets.md) plus a `slot`), `[[sidebar]]`,
`[[themes]]` (a `path` to a directory with `colors.toml`), `[[agents]]`,
`[[build]]` (killed after five minutes), and `[[startup]]`.
`platforms = ["linux", "macos"]` limits the plugin or one entry.

### What each process gets

All run with the plugin directory as working directory and get
`PHUX_PLUGIN_ID` and `PHUX_PLUGIN_ROOT`.

- **Actions** get a scrubbed environment: only `PATH`, `HOME`, `LANG`,
  `LC_ALL`, `TMPDIR`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`,
  and `PHUX_BIN`, plus `PHUX_PLUGIN_ACTION_ID` and `PHUX_ACTION_CONTEXT`. On
  Unix `PHUX_SOCKET` is deliberately unusable, so a nested `phux` does not
  silently reach your server.
- **Event hooks** get a [config hook's environment](./hooks.md#matching)
  plus `PHUX_PLUGIN_EVENT_ID`.
- **Panes** are ordinary panes with `PHUX_PLUGIN_PANE_ID`, `PHUX_SOCKET`,
  and `PHUX_TERMINAL_ID`.
- **Startup sidecars**: see [below](#startup-sidecars).

A plugin's `exec` widget runs in the client with no plugin variables.

## Running actions

An action runs on the machine that invokes it: the TUI palette or a key,
Rune's palette, `phux config run`, or MCP.

```sh
phux config run example.pane-tools summarize \
  --target-json '{"host":"hub","resource_handle":"worker/@7"}' \
  --input-json '{"format":"short"}'
```

It prints the action's output and exits with its code. Bind one yourself
with `plugin-action` ([Key bindings](./keys.md#tables)); `input = { ... }`
in the binding passes JSON input.

`PHUX_ACTION_CONTEXT` is versioned JSON (at most 16 KiB) with
`schema_version`, `executor`, `source`, `target`, `input`, and `observed`.
The UI captures the target before the job starts, so an action must not
look up the client's later focus. Missing or ambiguous target facts are
absent, never guessed. An action that needs the server must call a public
operation from an authorized client.

## Startup sidecars

A `[[startup]]` entry is a long-running process the server supervises for
its lifetime, such as a local web server.

```toml
[[startup]]
id = "web"
command = ["bun", "server.ts"]
```

- At most 8 per manifest and 8 across all plugins, in `[[plugins]]` order.
- Started once the server accepts connections, including after `phux
  upgrade`. Environment is the server's minus pane identity, plus
  `PHUX_SOCKET`, `PHUX_PLUGIN_NAME`, `PHUX_PLUGIN_VERSION`, and
  `PHUX_PLUGIN_STATE` (a private state directory). Stdin is `/dev/null`;
  output goes to a rotated log ([files](./reference/files.md)).
- A failure restarts it with backoff from 1 to 30 seconds; the sixth failure
  in ten minutes gives up until the next start or a disable/enable. Exit 0
  is a deliberate stop.
- On server exit, upgrade, or disable, the process group gets `SIGTERM`,
  then `SIGKILL` after 5 seconds. Orphans from a killed server are reaped on
  the next start.

**A sidecar has owner-level access to `PHUX_SOCKET`**, and while connected
it keeps an idle server alive.

## Listing a plugin

Give the repository the GitHub topic `phux-plugin` and a root
`phux-plugin.toml` that `phux plugin validate` accepts. A repository with
several plugins lists them in `phux-plugin-dirs.toml`
(`subdirs = ["plugins/pane-tools"]`). The index document is
[plugin-index.schema.json](./spec/plugin-index.schema.json).

## First-party plugins

[`plugins/`](../plugins/README.md) holds the optional first-party plugins;
the starter [distro](./distros.md) enables some of them. For a minimal
plugin to copy, see the [theme example](../examples/plugins/theme-pack/README.md).

## Debugging

- `phux plugin validate` checks every configured manifest, including an
  `on` that names no hook event.
- `phux config keys` lists plugin keys as `plugin <id>`.
- `phux plugin log [--failed]` shows recent action, hook, and sidecar runs
  with exit status and duration; `--startup [ID]` prints sidecar logs.

A missing action means the plugin is disabled or you haven't reloaded. A
hook that never logs means the server hasn't restarted since you enabled it.
