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

## Install

Install `phux` and `phux-mcp` first, then register and install the marketplace
plugin:

```sh
claude plugin marketplace add no-phux/phux
claude plugin install phux@phux
```

The plugin requires Claude Code 2.1.232 or a compatible newer release, and phux
0.16.0 or newer on `PATH`. Start a local phux server before using MCP tools.

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

This tree's `phux agent install-claude` shim (schema 5) registers every arm
below and emits AgentSession records through `phux agent session` /
`phux agent emit`. An older released binary's shim (schema 4) registers every
arm except `PreToolUse` and `PostToolUse`, feeds the detector with
`phux agent report-state`, and emits no records. Check `phux status --json`
for `RESOURCE_KINDS`.

On a server that advertises `RESOURCE_KINDS`, the shim gives every Claude run
inside a phux pane an **agent session**: a second resource, parented to the
pane, whose stream is one JSON record per hook event
([`agents.md`](./agents.md)). Every `--phux-hook` arm reads the
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

**Fallback.** Against a server without `RESOURCE_KINDS` — an older server, or
a hub relaying a pane it does not own — `session open` refuses with
`unsupported_server`, the shim notes that once, and the per-turn arms do what
the schema-4 shim does: `phux agent report-state` with `working`, `blocked`,
or `done`, feeding the detector directly. Identity at `SessionStart`,
`phux ask` on the blocking arms, and `clear` at `SessionEnd` run on every
server either way. Nothing is lost that was available before; the event log
is what is missing.

**Privacy.** Prompts are not forwarded: `prompt` carries a character count
and nothing else. Tool records carry the tool's name and never its input or
output. The hook's raw stdin JSON is emitted as `provider_raw` only when
`PHUX_AGENT_EMIT_RAW=1` is set in Claude's environment; it is off by
default and the shim never sets it. The retained stream lives in server
memory under `defaults.agent-log-bytes` (4 MiB per session by default), is
readable by any client on the socket through `phux agent log`, and is not
recorded by `phux rec` ([`recording.md`](./recording.md) §2).

The marketplace plugin's own hooks keep the identity-and-ask contract
described under Runtime contract until they are moved onto the same arms.

## Validation and versioning

`integrations/claude/package.json`, the plugin manifest, and the repository
marketplace entry share one component version. CI runs Anthropic's strict plugin
validator, package-shape tests, exact hook argv tests, and a high-severity npm
audit. Release Please owns `claude-plugin-vX.Y.Z`; the component release workflow
archives the exact tagged plugin and publishes the draft GitHub release only
after validation.
