---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-10-11
---

# iOS client

**TL;DR.** The iOS client is forthcoming. The requirements below are for
mobile integration; they do not establish release availability.

## Minimum `PHUX_REV`

`phux-mobile` consumes the `uniffi` lane of
[`crates/phux-client-ffi`](../../crates/phux-client-ffi) as one
revision-pinned artifact (ADR-0133, ADR-0135). Its pin must be at least
`7093116a955103cbd1a606e1fcfa2694c6a1625f` for the cwd/command/exit status
effects (`KernelStatus::Cwd`, `CommandStarted`, `CommandFinished`, `Exited`)
and `bbccc3ad88739d6c37aec985d2407dc41b92631a` for attach roles
(`RolePolicy`, ADR-0127; the `uniffi` lane declares it with
`set_attach_viewer`).
Neither surface has a mobile consumer yet.

## Terminal theme

`RemoteClient.setTerminalTheme(theme:)` installs a `WireTerminalTheme`
(exactly sixteen `ansi16` colours plus `foreground`, `background`, and
`cursor`) as the defaults every grid is resolved with (ADR-0157), now and
on every later `connect`; `nil` clears it. A program's OSC colour changes
still win over it. The mapping from a `colors.toml` is the one
[native SDK guide](./sdk.md#native-terminal-theme) lists, and `phux theme
show NAME --json` prints it as `ansi16`. Requires a `PHUX_REV` that
includes this surface.

## Agent session records

The `uniffi` lane streams every agent session (ADR-0103) running in a pane
the connection streams, and delivers its records as
`WireEvent.agentRecords` (Kotlin `WireEvent.AgentRecords`):

| Field | Meaning |
|---|---|
| `agentSessionId` | The `AgentSession` resource id. |
| `parentTerminalId` | The pane it runs in, as `topology()` names panes. |
| `provider`, `nativeId` | The provider (`claude`) and its own session id. |
| `kind` | `AgentRecordsKind`: `retained`, `live`, or `closed`. |
| `seq` | The newest record's server sequence, 0 when there is none. |
| `jsonl` | Complete `AgentEventsJsonlV1` records, one JSON object per line. |

`retained` replaces every record held for the session with the server's whole
retained stream, even when `jsonl` is empty. `live` appends incremental records:
either one live output frame or a committed reconnect suffix. Preserve the
prefix across a same-incarnation transport loss; do not clear it just because
the connection reopened. `closed` carries no records and ends the session:
it closed, its pane was detached, or the server restarted (after which its id
may name a new pane).

A `PHUX_REV` including [ADR-0159](../adr/0159-agent-streams-resume-from-committed-records.md)
resumes automatically when the server advertises `AGENT_STREAM_RESUME`.
An evicted cursor or an older server gets `retained` instead. A successful
empty resume emits no record event and leaves the prefix intact. The runtime
owns the cursor; mobile consumers must not infer it from packet arrival.

The parent and provider fields repeat on every event, so mapping a transcript
to its pane needs no join. Transcript entries are `provider_raw` records in
the `phux.transcript/v1` convention (ADR-0156). Generated bindings ship with
the native slices they match; an exhaustive Swift `switch` or Kotlin `when`
over `WireEvent` must name `agentRecords` when re-pinning. Resume adds no
new event case or enum value.
