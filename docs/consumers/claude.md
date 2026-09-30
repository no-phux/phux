---
audience: humans, agents, consumers, contributors
stability: evolving
last-reviewed: 2026-09-12
---

# Claude Code integration

**TL;DR.** The first-party `phux` Claude Code plugin exposes the authoritative
`phux mcp` tool server and publishes lifecycle identity and attention events for
Claude sessions running inside phux panes. It is versioned independently from
the phux binaries and distributed through this repository's Claude marketplace.

Use the **plugin** when you want Claude to have phux tools. Use the optional
**launch shim** when you want invoking `claude` to create or enter a phux
session automatically. They are not interchangeable: the plugin registers
tools and hooks; the shim changes how Claude starts.

## Install

Install `phux` and `phux-mcp` first, then register and install the marketplace
plugin:

```sh
claude plugin marketplace add no-phux/phux
claude plugin install phux@phux
```

The plugin requires Claude Code 2.1.232 or a compatible newer release, and phux
0.16.0 or newer on `PATH`. Start a local phux server before using MCP tools.

## First shared-terminal walkthrough

1. Complete the plugin installation above. Start `phux` in a real terminal,
   leaving a shell pane available for Claude to inspect.
2. Open another phux pane (press `Ctrl-A`, release, then `%`) and run `claude`
   there. Launching inside phux lets the hooks identify Claude's own pane;
   the shell it will operate should be a different pane.
3. Ask Claude: “Use phux to list the sessions and panes, then snapshot the
   idle shell pane. Do not send input yet.” Confirm the returned screen is
   the shell you see, not Claude's own interactive UI.
4. Ask it to run a harmless `pwd` in that selected shell pane using the phux
   tools. **Expected:** the result identifies the shell's working directory,
   and you can see the command in the human terminal view.
5. Quit Claude normally to end the conversation, or detach from phux with
   `Ctrl-A`, release, `d` to leave the running work alone. Detach does not
   stop Claude or the shell.

If Claude has no phux tools, confirm the plugin is installed and restart
Claude. If tools exist but cannot reach the server, follow
[agent connection recovery](../troubleshooting.md#an-agent-or-mcp-host-cannot-see-the-server).
Do not also register a duplicate phux MCP server when the plugin already
provides one. For an MCP-only setup without plugin hooks, use
[manual MCP registration](./mcp.md#registering-with-a-host).

For automatic launch behavior, run `phux agent install-claude` and follow
its instructions; inspect `phux agent install-claude --help` first if you
already manage your own Claude launcher.

### Develop the plugin

For development, load the checked-out package directly:

```sh
cd integrations/claude
npm ci
npm run gates
claude --plugin-dir .
```

## Runtime contract

The plugin contributes two native Claude components:

- `.mcp.json` launches `phux mcp`, so Claude receives the same version-matched
  tool catalog as every other MCP host.
- Lifecycle hooks declare `name=claude` and `kind=claude` once at session start,
  call `phux ask` for permission and elicitation prompts, and clear the record at
  session end.

Hooks are best effort and silent. They act only when `PHUX_TERMINAL_ID` identifies
Claude's own pane, never guess from phux focus, and never write a lifecycle
`state`. The server-side Claude detector remains authoritative for working and
blocked state.

The built-in `phux agent install-claude` shim remains available for users who
want plain `claude` to create or enter a phux session automatically. The plugin
does not replace that launch behavior; it provides native tools and lifecycle
integration for Claude sessions regardless of how they were started.

## What the hook shim emits

**Checkout behavior, not a minimum-release promise:** the source checkout's
`phux agent install-claude` shim (schema 5) registers every arm below and emits
AgentSession records through `phux agent session` / `phux agent emit`.
The older schema-4 released shim omits `PreToolUse` and `PostToolUse`, feeds
the detector with `phux agent report-state`, and emits no records. The plugin
minimum above does not imply schema-5 hooks. Check the running server with
`phux status --json` for `RESOURCE_KINDS` before relying on the event stream.

On a server that advertises `RESOURCE_KINDS`, the shim gives every Claude run
inside a phux pane an **agent session**: a second resource, parented to the
pane, whose stream is one JSON record per hook event
([agent CLI guide](./agents.md)). Every `--phux-hook` arm reads the
hook's stdin JSON and acts only when `PHUX_TERMINAL_ID` names Claude's own
pane. What each arm does:

| Hook | Emits | On every server | Fallback only (no `RESOURCE_KINDS`) |
|---|---|---|---|
| `SessionStart` | `phux agent session open @$PHUX_TERMINAL_ID --provider claude --native-id <session_id>`, then `session_start` | `phux agent set --name claude --kind claude` | |
| `UserPromptSubmit` | `prompt` with `{"chars": N}`; the text is not forwarded | | `phux agent report-state working` |
| `PreToolUse` | `tool_start` with `{"tool_name": "..."}`; `tool_input` is never sent | | (new registration; nothing) |
| `PostToolUse` | `tool_end` with `{"tool_name": "..."}` | | (new registration; nothing) |
| `PermissionRequest` | `ask` | `phux ask`, so the TUI and fleet chrome keep their exact timing | `phux agent report-state blocked` |
| `Notification` (the permission, idle-prompt, and elicitation matchers) | `notification` with the hook's `kind` | `phux ask` | `phux agent report-state blocked` |
| `Stop` | `stop` | | `phux agent report-state done` |
| `SessionEnd` | `session_end`, then `phux agent session close` | `phux agent clear` | |

The server derives the pane's lifecycle state from that stream ahead of every
other source (working on `prompt` / `tool_start`, blocked on `ask` and a
permission or elicitation `notification`, done on `stop`, retracted on
`session_end`), so `phux agent wait --until done` and the sidebar's state
glyph read the harness's own account of the turn rather than a screen rule.

**Fallback.** Against a server without `RESOURCE_KINDS` (an older server, or
a hub relaying a pane it does not own), `session open` refuses with
`unsupported_server` and the per-turn arms fall back to
`phux agent report-state`, as the table shows.

**Privacy.** Prompts are not forwarded: `prompt` carries a character count
and nothing else. Tool records carry the tool's name and never its input or
output. The hook's raw stdin JSON is emitted as `provider_raw` only when
`PHUX_AGENT_EMIT_RAW=1` is set in Claude's environment; it is off by
default and the shim never sets it. The retained stream is bounded by
`defaults.agent-log-bytes`, readable by any client on the socket through
`phux agent log`, and not recorded by `phux rec`.

The marketplace plugin's own hooks keep the identity-and-ask contract
described under Runtime contract until they are moved onto the same arms.

## Validation and versioning

`integrations/claude/package.json`, the plugin manifest, and the repository
marketplace entry share one component version. CI runs Anthropic's strict plugin
validator, package-shape tests, exact hook argv tests, and a high-severity npm
audit. Release Please owns `claude-plugin-vX.Y.Z`; the component release workflow
archives the exact tagged plugin and publishes the draft GitHub release only
after validation.
