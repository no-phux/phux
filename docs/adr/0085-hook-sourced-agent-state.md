---
audience: contributors
stability: stable
last-reviewed: 2026-08-14
---

# 0085 — Hook-sourced agent state is detector evidence

**TL;DR.** Hooks report working, blocked, and done to the server-side detector,
which publishes them immediately without disabling later process or screen
correction.

Status: Accepted
Date: 2026-08-14
Builds on: ADR-0040 (agent metadata), ADR-0046 (server-side detection), and
ADR-0061 (capability-gated wire additions)
See [ADR-0103](./0103-agent-session-resource-and-producer-fed-streams.md) for the current scope: the hook path is a fallback below the AgentSession stream.

## Context

Lifecycle hooks know exactly when an agent starts a turn, blocks, and finishes.
Writing those values directly into `phux.agent/v1` is nevertheless wrong:
declared state stands the detector down for the record's lifetime, so a missed
cleanup hook leaves a permanent badge and later screen evidence cannot heal it.
Removing per-turn record writes restored self-healing but removed the only
honest producer of `done` and added detector-tick latency to hook-known edges.

## Decision

`REPORT_AGENT_STATE` is an additive, capability-gated L1 command carrying a
Terminal and one of `working`, `blocked`, or `done`. It feeds the pane's
existing detector; it never writes metadata directly.

After the detector identifies an agent, accepted hook evidence publishes
immediately through the ordinary detector-to-metadata arbiter and updates the
detector's edge filter. A hook that narrowly beats the first identity poll is
held for at most 1.5 seconds and published when identity resolves; it cannot
attach to an unrelated future occupant. The actor then forces one normal
screen derivation, so subsequent process and screen evidence can supersede the
hook. Hook evidence is priority, not a latch.

`idle` is deliberately absent. A hook can state that a turn ended (`done`),
but it cannot prove that the interactive process is available for input after
its callback returns. The detector retains ownership of idle and departure.

The command uses tag `0x17` and `REPORT_AGENT_STATE = 0x00000400`; protocol
version advances from `0.7.0` to `0.8.0`. Clients must observe the feature bit
before sending it. The CLI ingress is `phux agent report-state TARGET STATE`.

The Claude shim keeps its one identity-only metadata write at `SessionStart`.
`UserPromptSubmit`, permission/notification, and `Stop` hooks report working,
blocked, and done respectively; blocked hooks continue to emit `phux ask`.

## Consequences

- `done` again has an honest producer and `agent wait --until done` is useful.
- Missing or delayed hooks cannot permanently suppress detector correction.
- A report arriving just before process identification survives the startup
  race without manufacturing identity from an unauthenticated state word.
- Integrations gain a wire-facing hook verb, so older servers fail closed via
  capability negotiation instead of silently dropping the command.

## Alternatives

**Write state metadata from hooks.** Rejected because declarations disable the
self-healing detector.

**Add positive screen rules for done.** Rejected because completion is a
lifecycle fact that most terminal screens cannot distinguish from idle or a
crash.

**Let hooks report identity too.** Rejected because process inspection and
manifests already own identity; combining the claims would recreate the stale
record path under a different command name.
