---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-07-09
---

# 0040 — Agent identity and lifecycle are an L3 metadata record

**TL;DR.** An agent's identity and lifecycle — name, kind, state
(idle/working/blocked/done), attention, session label — live in one
normative L3 metadata record, `phux.agent/v1`, scoped to the Terminal the
agent runs in. It rides the existing `SET_METADATA` / `GET_METADATA` /
`SUBSCRIBE_METADATA` verbs (zero new wire surface, the ADR-0027 tags/links
precedent). OSC `phux-ask` titles and screen-scrape heuristics remain
compatibility fallbacks; the record, when present, outranks both.

Status: Accepted
Date: 2026-07-09
See [ADR-0103](./0103-agent-session-resource-and-producer-fed-streams.md) for the current scope: when an AgentSession exists, `phux.agent/v1` is a projection of its stream.

## Context

phux surfaces "what agent is in this pane and does it need me?" in three
places today, each with its own derivation: the `phux agent list/show/explain`
CLI infers identity and state from title/screen substrings; the TUI sidebar
labels windows with the raw OSC title; and plugin manifests declare static
`[[agents]]` records that never change at runtime. ADR-0035/0036 solved one
slice — the *blocked-for-a-question* moment — with the `phux-ask` title
sentinel and the opt-in `ReportAsked` hook, but full lifecycle (who is here,
working or idle, how urgent) still has no structured home. Every consumer
re-derives it with substring heuristics, and the heuristics disagree.

## Decision

1. **One record, one key.** `phux.agent/v1`, scoped to a `TerminalId`, holds
   a UTF-8 JSON object: `name` (required), `kind`, `state`, `attention`,
   `session` (all optional; `state`/`attention` are OPEN string enums —
   unknown values read as `unknown`, never a parse failure). The schema is
   **normative** (`docs/spec/L3.md` §3.7), exactly like `phux.tags/v1`: the
   server stores the bytes opaquely; the *consumers* are constrained so
   meaning cannot drift.
2. **Existing verbs only.** Writers use `SET_METADATA` / `DELETE_METADATA`
   (the `phux agent set/clear` CLI, an agent hook, a plugin, a provider
   integration — anything that can reach the socket). Readers use
   `GET_METADATA` + `SUBSCRIBE_METADATA`. No new frame, tag, event kind, or
   version bump; the spec change is a draft-level convention entry.
3. **Record outranks heuristics.** A consumer that finds `phux.agent/v1` MUST
   prefer it over title/screen inference. The `phux agent` CLI reports it as
   its top-confidence source; the TUI sidebar labels the window from it.
   Absent the record, the OSC-title path and the detector heuristics behave
   exactly as before — the compatibility bridge stays.
4. **Terminal scope is the association.** The terminal association is the
   key's scope itself; the session association is the snapshot's
   terminal-to-session mapping plus the optional free-form `session` label
   for agent-defined grouping (a Herdr-style fleet name). No second identity,
   no registry.

## Why

The design space this closes: agent lifecycle could have been (a) a new
`AgentEvent` variant per state change, (b) server-interpreted state attached
to the Terminal, or (c) client-convention metadata. (a) grows the wire for
every vocabulary change and gives late subscribers nothing to read back.
(b) violates the L3 contract ("the server MUST NOT interpret metadata") and
ADR-0017's dumb-server stance. (c) — this ADR — gets read-back, change
push, per-terminal lifecycle cleanup (the store already drops a closed
Terminal's keys), and cross-consumer composition for free, on verbs that
shipped in v0.3. It is the same shape ADR-0027 chose for tags and links, and
that choice has held.

## Tradeoffs

- The record is only as truthful as its writer; a crashed agent can leave a
  stale `working`. Mitigation: the key dies with the Terminal, consumers show
  provenance (`agent_record` vs `screen`), and heuristics still run when the
  record is absent. A TTL field is an additive follow-up if staleness bites.
- Whole-record writes (last writer wins) rather than field merges. Fine for
  one agent per pane — the intended shape — and radically simpler than merge
  semantics on opaque bytes.
- Static plugin `[[agents]]` declarations stay declarative templates; the
  live feed (phux-r82.10) writes this record rather than mutating manifests.

## Alternatives

**New `AgentEvent::StateChanged` on the event stream** — rejected: events
are push-only convenience signals (ADR-0035); identity needs read-back state,
and an open vocabulary on a positional wire body is a versioning trap.

**Server-side typed agent registry** — rejected: the server would interpret
and validate agent semantics, coupling it to a vocabulary that is still
evolving; L3 explicitly exists so it does not have to.

**Extend the `ReportAsked` command into a general state verb** — rejected:
`ReportAsked` is a moment (a pending question), not a state store; widening
it duplicates what `SET_METADATA` already does with worse read-back.
