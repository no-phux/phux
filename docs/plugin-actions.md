# Running plugin actions

Phux runs a configured `[[actions]]` command as a separate process on the
invoking machine. Invoke it from the TUI palette or key binding, the Rune
desktop palette, CLI, or MCP.
The default process working directory is the plugin directory.

```sh
phux config run example.pane-tools summarize
phux config run example.pane-tools summarize \
  --target-json '{"host":"hub","satellite":"worker","resource_handle":"worker/@7"}' \
  --input-json '{"format":"short"}'
```

`--target-json` and `--input-json` supply data to headless actions. MCP
accepts the same data in `action_target` and `input`, and does not accept an
alternative config or working directory. Neither adapter silently attaches
to a server or infers a selection from its environment. `phux config run`
prints the action's output and exits with its exit code. `--timeout SECS`
gives up after that long; `--json` prints the result as a JSON document.

## Process environment

| Variable | Value |
| --- | --- |
| `PHUX_PLUGIN_ID` | The plugin's manifest `id` |
| `PHUX_PLUGIN_ACTION_ID` | The action's `id` |
| `PHUX_PLUGIN_ROOT` | The plugin directory |
| `PHUX_ACTION_CONTEXT` | Versioned JSON context, up to 16 KiB |

The action receives `schema_version: 1`, `executor: "invoking_machine"`,
`source` (`cli`, `mcp`, `tui_palette`, `tui_key`, `tui`, or `rune_palette`),
`target`, and `input`. `observed` distinguishes client observations from
caller-supplied target data. The UI captures context before starting the job;
an action must not resolve the client's later focus. `client`, if available,
is the invoking connection's id. The target can carry `host`, `route`,
`transport`, `satellite`, `authority`, `workspace`, `session`,
`session_handle`, `session_name`, `resource`, `resource_handle`,
`activation`, `instance`, `arrangement`, `group`, and `entry`. Unavailable
or ambiguous facts are absent. A handle alone is not a durable address.
A selected remote route describes the data, not where the process will run.

Rune reloads the local catalog when you open the palette. It does not use
TUI manifest chords, and the phone UI does not start local plugin processes.

Actions do not inherit the caller's phux socket, pane environment, remote
credentials, or arbitrary environment variables. On Unix a deliberately
unusable `PHUX_SOCKET` prevents nested `phux` commands from automatically
selecting the owner socket. This does not restrict a trusted program that
explicitly locates and opens a socket it already has OS permission to use.
The allowlist is `PATH`, `HOME`, `LANG`, `LC_ALL`, `TMPDIR`,
`XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, and `PHUX_BIN`.

Actions that previously assumed a live server socket must use the relevant
public server operation from an authorized client instead. The packaged
Continuum hooks still save archives; use `phux archive export` or
`phux archive import` for explicit archive operations. Agent stats and usage
publishing likewise keep their hooks; use `phux agent stats` or
`phux agent usage --refresh --publish` explicitly on the intended server.

## Keys

To give a plugin action a key, set `keys` in its manifest, or bind it in
your own `[keybindings]` with the `plugin-action` action, naming the
plugin and the action's `id`:

```toml
[keybindings.prefix-table]
"Y" = { action = "plugin-action", plugin = "example.pane-tools", id = "summarize" }
```

Use `input = { format = "short" }` in the binding to pass JSON-compatible
data to an action. Your bindings win over a manifest's `keys`. Binding
the same key yourself takes it back, and a key you set to `"none"` stays
unbound even when a plugin's `keys` names it.
