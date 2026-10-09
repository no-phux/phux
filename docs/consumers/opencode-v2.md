---
audience: humans, agents, consumers
stability: evolving
last-reviewed: 2026-09-30
---

# OpenCode V2

**TL;DR.** The private OpenCode V2 plugin exposes phux terminal and agent tools
through the native OpenCode tool API. phux owns the PTYs; OpenCode retains
permissions, transcripts, and sessions. Tool selections are session-local, and
lifecycle identity stays on the hosting pane rather than following a worker.

## Installation and supported contract

The adapter lives in [`integrations/opencode-v2`](../../integrations/opencode-v2/README.md).
That package README is the installation, configuration, tool catalog, and
verification reference, including local tarball installation. It pins the public
`@opencode/plugin@2.0.26` V2 API; it is not the legacy V1 plugin contract.

## First shared-terminal walkthrough

This source-checkout integration requires a trusted checkout and a compatible
OpenCode V2 host. There is no published package or V1 adapter; installing
phux alone does not install it. For a published integration, use
[MCP or another host](./getting-started.md).

1. Install phux and complete the [local quickstart](../QUICKSTART.md). Install
   and authenticate OpenCode separately.
2. In your phux checkout, build the plugin:

   ```sh
   cd integrations/opencode-v2
   bun install
   bun run build
   ```

   OpenCode imports the generated `index.js`, not the TypeScript source.
3. Use the checkout's `opencode.jsonc`, or copy the local-package configuration
   from the [plugin load instructions](../../integrations/opencode-v2/README.md#build-and-load)
   into your project's OpenCode configuration, changing the absolute package
   and socket paths to your own. This is source loading, not an npm install.
4. Add the package to global `cli.json` as described in the package README,
   then start `opencode` in a phux pane so its terminal-side companion inherits
   `PHUX_TERMINAL_ID`. Ask it to
   use `phux_list`, then `phux_create` to create a sibling shell for work.
5. Ask for a `phux_snapshot` of that sibling before writing. **Expected:** the
   returned screen is a shell, not the OpenCode TUI. Then ask it to run `pwd`
   there with `phux_run`; the shell result is visible to both you and OpenCode.

The plugin refuses `phux_run` and `phux_send_keys` aimed at its own parent
pane. Keep that guard: use a sibling, not `--force` or OpenCode's built-in
terminal. If tools are absent, check the plugin build and local-package path;
if connection fails, check the configured local socket with
[agent connection recovery](../troubleshooting.md#an-agent-or-mcp-host-cannot-see-the-server).
Quit OpenCode normally to end it, or press `Ctrl-A`, release, then `d` to
detach while leaving it running.


The distributable bundles the shared phux TypeScript runtime into `index.js`
and includes standalone type declarations. The OpenCode SDK is its declared
external dependency. A checkout is needed to build, not to load the built
package. This package remains private and is not published to a public registry.

## Ownership boundary

- OpenCode native permission policy still controls tool execution. This plugin
  does not approve requests, bypass hooks, or replace the host's shell executor.
- The hosting terminal is identified by the TUI's `PHUX_TERMINAL_ID`, not a
  shared background server's environment. Controlled siblings are not labelled
  as OpenCode. A dedicated server may explicitly opt into server lifecycle
  reporting with `serverLifecycle: true` and a fixed `PHUX_TARGET` identity;
  do not combine it with the CLI companion on the same pane.
- Tool-created targets belong to the calling session's selection. Deleting that
  session removes the selection and its cached context. Other sessions keep
  their own selections.
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
