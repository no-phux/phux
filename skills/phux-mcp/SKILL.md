---
name: phux-mcp
description: Drive phux through its installed MCP stdio adapter. Covers live schema discovery, the read-act-observe-verify loop, selectors, agent lifecycle edges, delivery uncertainty, bounded waits, and destructive-operation confirmation.
---

# Driving phux through MCP

`phux mcp` is the installed entry point for the stdio MCP server. Configure an
MCP host to launch `phux` with `mcp` as its first argument. The launcher
replaces itself with the bundled `phux-mcp` implementation, preserving stdio,
signals, and status. Direct `phux-mcp` configurations remain valid.
`--skill` and `--schema` are standalone discovery endpoints, not server
arguments.

Inside MCP, the live `tools/list` catalog is authoritative. Outside an MCP
session, run:

```sh
phux mcp --schema
```

It prints the exact same tool descriptor array, including every current
`inputSchema`, compiled into this binary. Do not infer required fields, limits,
or enums from examples when the catalog can answer them.

`phux-mcp` finds the sibling `phux` executable first, then `phux` on `PATH`.
`PHUX_SOCKET` selects the default local server; a tool's `socket` argument
overrides it. Installing or registering MCP does not imply a phux server is
running.

## Operating loop

Read, act, observe under a finite bound, then verify with another read.

1. Discover sessions with `phux_ls`.
2. Create or place work with `phux_new`, `phux_launch`, `phux_spawn`, or
   `phux_agent_start`.
3. Read with `phux_snapshot`, `phux_agent_show`, or `phux_agent_explain`.
4. Use `phux_run` for one shell command, `phux_paste` plus `phux_send_keys`
   `Enter` for multiline input, and `phux_agent_prompt` for an agent turn.
5. Observe with `phux_wait`, bounded `phux_watch`, or `phux_agent_wait`.
6. Re-read state. A watcher ending or a quiet pane is not completion.
7. When something is wrong rather than merely slow, ask `phux_status` or
   `phux_doctor` before guessing. Both are read-only.

Prefer returned direct selectors such as `@7` and `host/@7` for writes. Session
names and `#tags` can select sets. `.` means the focused session. `%name` is
reserved but no shipped MCP tool resolves it yet; use `@N`. `=` is unavailable
because headless MCP has no attached client's focus history. MCP layout tools
change persisted topology but never move a human's client-local focus.

## Agent lifecycle and delivery

`phux_agent_show` and `phux_agent_list` are level reads: what appears true now.
`phux_agent_wait` is an edge read: it requires an observed transition after its
baseline. An already-idle pane is not proof that a turn completed. Prefer
`phux_agent_prompt` when submitting work because it fuses the write and edge
observation without a race.

Always set finite timeout values. A timeout says no qualifying transition was
observed; it does not prove the agent is still working. `unknown` or a removed
record is departure, never completion.

An acknowledged input write proves bytes entered the kernel tty queue, not that
the application consumed them. A `delivery_unknown` result is terminal: inspect
the pane and do not resend, because the first operation may still land. Use
identity-checked `phux_agent_send_keys` or `phux_agent_prompt` when stale pane
occupancy would be dangerous. Serialize acknowledged fleet prompts because the
input lane is server-scoped. A paste inserts text; it does not submit it.

## Tool inventory

Read exact arguments from `tools/list` or `phux mcp --schema`.

**Read and observe:** `phux_ls`, `phux_snapshot`, `phux_wait`, `phux_watch`,
`phux_agent_list`, `phux_agent_show`, `phux_agent_explain`, `phux_agent_wait`,
`phux_agent_log` (the retained agent-session event stream, one bounded read;
there is no follow argument).

**Diagnose:** `phux_status` (one server: running, pid, uptime, protocol,
sessions, log paths) and `phux_doctor` (the whole install: config, socket,
server, server-health, plugins, shim, logs). Both are read-only and start
nothing. A stopped server and a failed check are answers, not tool errors:
branch on `running` and on `ok` plus each check's `status`, never on whether
the call succeeded. `phux_doctor` check names repeat — read every
`server-health` row, because a crash-loop, a legacy supervisor unit, and
version skew co-occur. A null `pid` on a running server is a peer-credential
gap, not a stopped server. Relay a `hint`; do not run it. `phux_whoami`
reports who this connection is to that server (principal, credential id, auth
route, peer uid, serving user, host). It only reads, and a server too old to
report identity is refused with `server_too_old` rather than guessed.

**Create and act:** `phux_new`, `phux_launch`, `phux_spawn`, `phux_run`,
`phux_send_keys`, `phux_paste`, `phux_ask`, `phux_signal`, `phux_tag`,
`phux_rename`, `phux_agent_set`, `phux_agent_clear`, `phux_agent_send_keys`,
`phux_agent_prompt`, `phux_agent_answer`, `phux_agent_start`.

**Agent sessions:** `phux_agent_session_open` binds an agent-session resource
to a pane and makes the caller its producer; `phux_agent_emit` appends one
typed record (the server stamps `seq` and `ts_ms`); `phux_agent_session_close`
closes the session and leaves the pane alone. On a server without
`resource_kinds` in `phux_status`'s `features`, each returns
`unsupported_server`.

**Shape and extend:** `phux_insert_pane`, `phux_move_pane`, `phux_swap_pane`,
`phux_workspace`, `phux_plugin_action`, `phux_plugin_workspace`.

**Destructive boundary:** `phux_kill` and `phux_detach` require `confirm: true`.
Before either one, resolve and display the exact target, inspect current state,
explain the effect, obtain affirmative human confirmation, execute the narrowest
operation, and verify the resulting inventory. `phux_detach` ejects viewers but
does not kill panes. Treat destructive `phux_signal` values the same way.

## MCP results

Tool success and tool failure are JSON-RPC successes carrying one text content
block. Structured successful text is JSON. Tool failures set `isError: true`;
they are not protocol errors. Malformed requests and unknown methods use
JSON-RPC errors.

There is deliberately no attach/live-ANSI tool, headless focus tool, durable
input lease, scheduler, credential mutation surface, or endless watch call.
Those omissions keep agent control explicit and bounded.
