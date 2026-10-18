# @phux/opencode

OpenCode V2 plugin, tested against the public `@opencode/plugin@2.0.32` contract.
OpenCode keeps its own shell, sessions, and permissions; phux supplies
terminals only where a terminal is the point.

## Install

```sh
opencode plugin add @phux/opencode
```

This needs `@phux/opencode` 0.4.0 or newer; 0.3.0 and earlier are the retired
OpenCode V1 adapter.

The CLI automatically loads the terminal companion from the server plugin.
Leave the version off so updates reach
it: OpenCode never updates plugins on its own, so run `opencode plugin check`
and `opencode plugin update` (or the TUI's Plugins dialog) to move to the
newest release.

## Build and load from a checkout

From a phux checkout:

```sh
cd integrations/opencode-v2
bun install --frozen-lockfile
bun run compile
```

OpenCode loads bundled `index.js` for server tools and `tui.js` for the terminal
companion, not TypeScript sources. The bundles include phux's shared runtime;
the TUI entrypoint is universal-compiled against OpenTUI 0.5.17 and Solid 1.9.12.
Install the declared peers before loading a checkout; merely having `tui.js`
on disk does not mean its runtime dependencies are available.
Artifacts are checked in and Git installs require no build lifecycle hooks;
the compiled OpenCode installer cannot run npm Git preparation.
Configure OpenCode V2 with an absolute package directory:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "plugins": [
    {
      "package": "/absolute/path/to/phux/integrations/opencode-v2",
      "options": { "socket": "/absolute/path/to/phux.sock" }
    }
  ]
}
```

Omit `socket` to use the phux CLI's environment/default selection. `phux` must
be available on PATH, or set the plugin's `executable` option to its path.
Configure the package once: globally if you want phux tools in every project,
or in a project's `opencode.jsonc` if you only want them there. Do not register
both the primary checkout and a worktree copy in the same location: OpenCode
loads both plugin entries, and a stale bundle can override the newer tools.

The companion runs inside each CLI, so the shared OpenCode service does not
need to know which phux pane is displaying a session. It claims only the
session visible in this TUI and clears the prior
claim on navigation or exit. This is what lets phux-mobile link a pane to its
native OpenCode view by exact `opencode:<session id>` identity:

For a CLI connected to a remote service without this plugin, load the companion
locally through `cli.json` using the same package target. Do not also register a
different local checkout when the connected service already supplies it.

The CLI companion does nothing outside a phux pane. The server plugin may
still supply tools globally, but **do not assume its `PHUX_TERMINAL_ID` names
the active TUI pane** when the service is shared. Server-side lifecycle is off
by default to avoid competing claims; `serverLifecycle: true` is only for a
dedicated server permanently bound to one pane without the CLI companion.
Tools need an explicit target until this OpenCode session selects a spawned pane
or the service has a fixed `PHUX_TARGET`.

Releases publish to npm as `@phux/opencode` with provenance (tags
`opencode-plugin-vX.Y.Z`).

## When the agent uses phux

OpenCode's built-in shell is a local subprocess with native background jobs and
completion notifications. It is the right tool for builds, tests, git, and
nearly every command, so the plugin leaves it alone. The phux tools are for the
cases a subprocess cannot cover:

- an interactive program the agent must drive (REPL, TUI, debugger);
- a long-running process the user should be able to watch in a pane;
- another agent's pane to observe or prompt.

A stable system rule says this every request. It never names a pane, so it does
not disturb prompt caching.

## Tools and safety

Tools register through OpenCode's native tool transform API as Code Mode tools,
with a native permission action for each tool name:

- Discovery: `phux_panes`.
- Owned terminals: `phux_spawn`, `phux_kill`.
- Terminal control: `phux_run`, `phux_send_keys`, `phux_paste`.
- Observation: `phux_snapshot`, `phux_wait`.
- Agent control: `phux_agent_prompt`, `phux_agent_wait`, `phux_resource_wait`.
- Diagnostics: `phux_status`.

The shared runtime's `phux_create`, `phux_list`, and `phux_runtime_info` are not
registered here. Creating a named session per task left sessions behind, and the
other two duplicate `phux_panes` and `phux_status`.

A pane from `phux_spawn` belongs to the OpenCode session that spawned it. It
becomes that session's default target and carries the `opencode-owned` tag. Each
session may own `maxTerminals` live panes (default 4); further spawns are refused
until one exits or is closed with `phux_kill`. `phux_kill` closes only panes the
calling session owns. Deleting the session, or shutting down the plugin, kills
its panes. After a crash, `phux kill '#opencode-owned'` clears strays.

Use direct `@N` or `host/@N` selectors. Target precedence is an explicit tool target,
that session's selection, then `PHUX_TARGET`. Session deletion forgets its
selection. Output is bounded; run and wait operations default to a 30-second
phux deadline plus a 5-second local subprocess allowance. Short calls default
to 10 seconds. Native tool cancellation propagates to the CLI subprocess.

When the server itself is launched in phux, `PHUX_TERMINAL_ID` identifies its pane. Phux tools
refuse shell input, key injection, paste, and agent prompts into that pane;
reads are allowed. Use OpenCode's built-in shell for ordinary commands. Lifecycle metadata stays on
the hosting pane and never follows a selected worker. For a standalone launch,
an explicit `PHUX_TARGET` is the fixed lifecycle identity and initial tool
target; without either identity, no pane is labelled OpenCode.

Permission requests remain in OpenCode. The plugin observes permission events
but never approves them or rewrites host permission policy. Lifecycle records
contain identity and event metadata, not prompt bodies or a forced agent state.
Only one OpenCode session at a time owns the hosting pane's lifecycle stream.
The CLI companion keeps that owner in sync with the visible tab even when the
service is shared or multiple terminal clients show different sessions.

Fleet context is off by default; `phux_panes` answers on demand. Set
`PHUX_CONTEXT_AWARENESS=1` or `contextAwareness: true` to add a full fleet
checkpoint to every request. It costs one `phux agent list` per request and
changes the system prompt whenever any pane changes state. Options also include
`maxTerminals`, `lifecycleTimeoutMs`, and `contextTimeoutMs`.

The plugin does not hook OpenCode's shell, replace the terminal widget or PTY
API, or tunnel the OpenCode server. Use `phux attach` for a remote view.
Satellite targets work through the phux CLI and its configured local hub socket.

## Verification

```sh
bun run gates
```

The package gate typechecks, builds, then tests. Its packed-artifact regression
loads the tarball outside the checkout and checks registration against the
public SDK. Source tests cover cross-session selection/deletion, terminal
ownership and release, lifecycle identity, parent write protection, and
cancellation.

The checkout's paired Seroval overrides keep its dependency audit clean. npm
does not apply a dependency's overrides in a consuming project: a fresh tarball
install still reports Seroval advisories through the host-pinned Solid 1.9.12.
Fixing that consumer graph requires a compatible host update, not independently
upgrading the plugin's Solid, OpenTUI, or SDK internals.
