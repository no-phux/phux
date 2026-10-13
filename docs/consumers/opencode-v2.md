---
audience: humans, agents, consumers
stability: evolving
last-reviewed: 2026-10-10
---

# OpenCode V2

**TL;DR.** The `@phux/opencode` plugin exposes phux terminal and agent tools
through the native OpenCode tool API. OpenCode keeps its own shell, permissions,
transcripts, and sessions; the agent reaches for phux only for interactive
programs, watched processes, and other agents' panes. Panes it spawns belong to
its session and close with it. Lifecycle identity stays on the hosting pane.

## Installation and supported contract

The adapter lives in [`integrations/opencode-v2`](../../integrations/opencode-v2/README.md).
That package README is the installation, configuration, tool catalog, and
verification reference. It pins the public `@opencode/plugin@2.0.26` V2 API;
it is not the legacy V1 plugin contract. Releases publish to npm as
`@phux/opencode` from `0.4.0`; `0.3.0` and earlier are the retired V1 adapter.

```sh
opencode plugin add @phux/opencode
```

Leave the version off the entry. OpenCode never updates plugins on its own:
`opencode plugin check` lists outdated ones and `opencode plugin update` moves
them to the newest release.

## First shared-terminal walkthrough

This integration requires a compatible OpenCode V2 host; installing phux
alone does not install it.

1. Install phux and complete the [local quickstart](../QUICKSTART.md). Install
   and authenticate OpenCode separately.
2. Install the plugin with `opencode plugin add @phux/opencode`, globally for
   all projects or in one project's configuration for that project only. To
   load a checkout instead, follow the
   [checkout load instructions](../../integrations/opencode-v2/README.md#build-and-load-from-a-checkout);
   do not load both the published package and a checkout at once.
3. Add the package to global `cli.json` as described in the package README,
   then start `opencode` in a phux pane so its terminal-side companion inherits
   `PHUX_TERMINAL_ID`. Ask it to run `pwd`. **Expected:** it uses its built-in
   shell; no phux pane appears.
4. Ask it to start a Python REPL you can watch. **Expected:** it calls
   `phux_spawn` with `python3`, and the pane shows up in `phux ls`. Ask it to
   evaluate `1 + 1` there and close the REPL. **Expected:** it sends keys,
   reads the result with `phux_snapshot`, then calls `phux_kill`.

The plugin refuses `phux_run` and `phux_send_keys` aimed at its own parent
pane, and `phux_kill` aimed at a pane its session did not spawn. If tools are absent, check the plugin build and local-package path;
if connection fails, check the configured local socket with
[agent connection recovery](../troubleshooting.md#an-agent-or-mcp-host-cannot-see-the-server).
Quit OpenCode normally to end it, or press `Ctrl-A`, release, then `d` to
detach while leaving it running.


The distributable bundles the shared phux TypeScript runtime into `index.js`
and includes standalone type declarations. The OpenCode SDK is its declared
external dependency. A checkout is needed to build, not to load the built
package.

## Ownership boundary

- OpenCode native permission policy still controls tool execution. This plugin
  does not approve requests, bypass hooks, or hook the host's shell.
- The hosting terminal is identified by the TUI's `PHUX_TERMINAL_ID`, not a
  shared background server's environment. Controlled siblings are not labelled
  as OpenCode. A dedicated server may explicitly opt into server lifecycle
  reporting with `serverLifecycle: true` and a fixed `PHUX_TARGET` identity;
  do not combine it with the CLI companion on the same pane.
- Spawned panes belong to the calling session: they become its selection, count
  against its `maxTerminals` cap, and are killed when the session is deleted or
  the plugin shuts down. Other sessions keep their own selections and panes.
- Public V2 tool contexts provide `AbortSignal`; cancellation reaches the local
  CLI subprocess. Killing that subprocess does not promise to stop a command
  already running in a terminal. Mutations are not retried.

## Remote

The plugin dials a local phux socket (`PHUX_SOCKET` or the `socket` plugin
option). To control `host/@N`, use a local hub socket. OpenCode's `--server`
does not provide phux federation. Use phux attach to view the pane.

## Lifecycle

The CLI companion publishes the currently visible session, switching ownership
when OpenCode tabs change and clearing it on exit. It writes identity only,
never a declared `state`. Prompt text and tool input stay off the stream. The
server plugin continues to provide tools but does not report pane lifecycle by
default. See the [harness author guide](./harness.md).

The plugin does not attach a viewer inside OpenCode's terminal widget or proxy
its HTTP API. Use phux's own attach surface for terminal viewing; see
[the TUI consumer](tui.md).
