---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-09-13
---

# Emit agent lifecycle from a harness

**TL;DR.** Open an `AgentSession` on the pane the agent runs in, then
append events with `phux agent emit`. While that stream is live it is
lifecycle truth. Screen detection is the fallback for harnesses that do
not emit. Do not write detector `state` from hooks.

---

This page is the contract for a coding-agent harness (OpenCode, Pi, Claude,
or yours). Verb flags and JSON live in [`agents.md`](./agents.md) and the
generated [CLI reference](../reference/cli.md). The resource kind is
specified in [`../spec/L1.md`](../spec/L1.md).

## Rank

1. A live `AgentSession` stream. `prompt` / `tool_start` derive working;
   `ask` derives blocked; `stop` derives done; `session_end` retracts.
2. Identity-only `phux agent set --name` so the pane has a name. Do not
   pass `--state`; that stands the detector down for the life of the
   record.
3. The pane detector (`rules/*.toml`, OSC title, process identity) only
   when nothing is emitting.

`phux agent show` reports `stream` as the source when (1) decided the
state. `phux agent explain` is the evidence trail when it did not.

## The loop

The harness is inside a phux pane (`PHUX_TERMINAL_ID` and `PHUX_SOCKET`
are set). It becomes the producer: only the client that opened the
session may append.

```sh
# once per pane, when the agent session starts
phux agent session open --provider opencode --native-id "$SESSION" --json "@$PHUX_TERMINAL_ID"

# on each lifecycle edge
phux agent emit --type prompt --data '{"chars": 61}' "@$session"
phux agent emit --type ask --data '{"reason": "permission"}' "@$session"
phux agent emit --type stop "@$session"

# when the agent session ends; the pane is untouched
phux agent session close "@$session"
```

`--type` is a closed set: `session_start`, `prompt`, `tool_start`,
`tool_end`, `notification`, `ask`, `stop`, `session_end`, `state`,
`provider_raw`. The server stamps `seq` and `ts_ms`. A server without
`RESOURCE_KINDS` refuses before anything is written
(`unsupported_server`); keep identity-only in that case, do not invent
a detector state.

## What not to do

- Do not scrape your own TUI and write `phux agent set --state`. That is
  how a hook stands the detector down and then lies.
- Do not open a second session on the same pane unless you mean two
  producers. The server does not deduplicate.
- Do not treat `idle` from `phux agent show` as completion. Completion
  is an observed edge (`phux agent wait`). See [`agents.md`](./agents.md).

Shipped integrations: [`opencode.md`](./opencode.md), [`pi.md`](./pi.md),
[`claude.md`](./claude.md).
