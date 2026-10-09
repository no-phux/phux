---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-10-09
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

`retained` is the session's whole retained stream, delivered once per
subscription and again after a reconnect or a resync: replace every record
held for the session. `live` appends one output frame. `closed` carries no
records and ends the session: it closed, its pane was detached, or the
server restarted (after which its id may name a new pane). The parent and provider fields repeat on every
event, so mapping a transcript to its pane needs no join. Transcript entries
are `provider_raw` records in the `phux.transcript/v1` convention
(ADR-0156). The case is additive: generated bindings ship with the native
slices they match, so no version gates it, but an exhaustive Swift `switch`
or Kotlin `when` over `WireEvent` must name it when re-pinning.
