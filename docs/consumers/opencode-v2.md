---
audience: humans, agents, consumers, contributors
stability: evolving
last-reviewed: 2026-09-26
---

# OpenCode V2 integration

**TL;DR.** `@phux/opencode-v2` is the OpenCode plugin. phux owns the PTY.
OpenCode owns the session. The plugin is the AgentSession producer and the
sibling terminal tools. It does not own a second PTY and it does not tunnel
OpenCode's HTTP API. There is no V1 adapter.

---

## First shared-terminal walkthrough

**Applicability:** this is a source-checkout integration for OpenCode V2.
There is no published package or V1 adapter. Use a trusted checkout and a
compatible OpenCode V2 host; installing the phux CLI alone does not install
this plugin. If you need a published integration now, choose
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
   from the [plugin load instructions](../../integrations/opencode-v2/README.md#load)
   into your project's OpenCode configuration, changing the absolute package
   and socket paths to your own. This is source loading, not an npm install.
4. Start `opencode` in a phux pane so it inherits `PHUX_TERMINAL_ID`. Ask it to
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


## Ownership

```text
phux Terminal
  └── AgentSession   provider=opencode, native_id=<OpenCode session id>
```

The parent Terminal is the pane OpenCode is running in when `PHUX_TERMINAL_ID`
is set. That pane is the TUI. Shell work goes to a sibling created with
`phux_create`. `phux_run` and `phux_send_keys` refuse the parent. Reads do not.

OpenCode's built-in terminal stays unused. There is no plugin hook that
replaces `/api/pty`. A later CLI plugin may attach as a viewer of the sibling.
It must not be a second writer.

## Remote

Attach to the pane, including `host/@N` through a hub. Do not point an
OpenCode client at the other machine with `--server` and call that federation.
The plugin dials only a local phux socket (`PHUX_SOCKET`, or `socket` in
plugin options). A hub socket is still that local socket.

## Lifecycle

The plugin emits the closed AgentSession record types. It writes identity
only, never a declared `state`. Prompt text and tool input stay off the
stream. See the [harness author guide](./harness.md).

## Package

Source and load instructions: [OpenCode V2 plugin package](../../integrations/opencode-v2/README.md).

Not in this cut: replacing the OpenCode `shell` executor, a viewer panel, and
a published package.
