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
phux config reload          # apply the reloadable subset below
```

The file is not watched; apply changes according to their owner below.
Every section and setting is listed in the [config reference](./reference/config.md).

## When a change takes effect

| Owner | Apply boundary |
| --- | --- |
| TUI keybindings, theme, chrome/status, plugin UI rows, which-key, clipboard-write, notifications | Explicit config reload. Failure leaves the previous settings intact. |
| TUI sidebar geometry/hosts settings, mouse capture and predictive echo | Next attach; detach and invoke attach again, not just reconnect/reload and not a server restart. |
| Server policy mode, listeners, hooks and server-owned defaults | Read at server start/handoff; client reload does not replace them. |
| Hub satellites and reverse uplinks | The config-reload doorbell reconciles changed links. |
| Workload registry and bearer stores | Their own admission/revocation reload path, not a universal config reload. |

Use `phux config show --layers` and `phux config check` to diagnose the file.
Runtime toggles do not silently write it back. The TUI settings page edits
validated keys in the file; it is not a second source of authority. Do not
restart live workloads to test a client-only knob. Server changes use the
planned in-place handoff, not a cold stop/start; confirm the new settings
and [remote authority](./remote.md#connection-authority) afterwards.
The [TUI settings/tests](../crates/phux-tui/src/settings.rs),
[reload adoption](../crates/phux-tui/src/attach/driver/config_ui.rs),
[server startup](../crates/phux-server/src/runtime/mod.rs) and
[link doorbell](../crates/phux-server/src/runtime/client.rs) define the boundaries.

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

## Predictive echo

The TUI's predictive echo defaults on for remote non-loopback connections,
off for local/loopback ones. Set an explicit override for sensitive displays:

```toml
[experimental]
predictive-echo = false
```

After setting this false, **detach this TUI and invoke `phux attach` again
before typing secrets**. Config reload and automatic reconnect retain the
previous attach's prediction setting; neither disables an existing overlay.

Prediction changes only this client's display, never wire input, other
clients or recordings. It does **not** know the child's no-echo/password
mode: characters typed at a password prompt can briefly appear locally
until contradicted or the adaptive one-to-five-second display lifetime
expires. Disable it if that shoulder-surfing risk is unacceptable. Do not
assume server ECHO awareness or an initial confirmation gate on the primary
screen. [Dial defaults/tests](../crates/phux/src/commands/attach.rs) and the
[prediction state machine](../crates/phux-client-core/src/predict/state.rs)
own the behavior.

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
processes. This is not sandboxing: package build steps at install/update
execute as the invoking user. Inspect and trust the package before installing.

```sh
phux plugin install https://github.com/someone/phux-plugin
phux plugin list
phux plugin disable NAME
```

[`examples/plugins`](../examples/plugins) has working plugins to copy.
[`usage`](../examples/plugins/usage) is the pattern for publishing a host
fact to every client: its hooks run a command that writes a Global metadata
key ([plan usage](./agents.md#plan-usage)).

## Projects

A repository can carry `.phux/project.toml`, describing the tabs and panes
to open for it. phux runs it only after you approve its exact contents.

```sh
phux project init       # write .phux/project.toml for this repository
phux project trust      # approve it
phux project open       # open the project's session, or attach if it exists
```

Edit the file and you have to `phux project trust` it again.

Sessions created inside a git checkout group by repository in the sidebar,
the session picker, and `phux ls`, across machines. To put any other
session in a project, link it by name; the link is stored on the server
that owns the session, so every client sees it:

```sh
phux project link notes phux     # group session "notes" with the phux sessions
phux project unlink notes        # back to its repository, or ungrouped
```

A link outranks the repository, and a link named like a repository joins
that repository's group.

## Layers

A config file can declare `extends = ["path/to/other.toml"]` to build on
other files. The layers apply in order: built-in defaults, then each
extended file, then yours. A key named `x-append` holding an array appends
to `x` instead of replacing it. `phux config show --layers` shows where each
value came from. `phux config init --distro starter` writes a config that
extends [`distros/starter`](../distros/starter), which enables the example
plugins.
