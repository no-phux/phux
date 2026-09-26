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
stream. See [`harness.md`](./harness.md).

## Package

Source and load instructions: [`../../integrations/opencode-v2/README.md`](../../integrations/opencode-v2/README.md).

Not in this cut: replacing the OpenCode `shell` executor, a viewer panel, and
a published package.
