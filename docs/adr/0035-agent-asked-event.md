---
audience: contributors
stability: stable
last-reviewed: 2026-06-17
---

# 0035 — Agent-asked event: a pending human-answerable question on the wire

**TL;DR.** An agent running in a pane that has blocked for a human answer
emits an additive `AgentEvent::Asked` on the existing agent-event stream
(`EVENT`, `0xB3`) carrying `{ id, question, suggestions, elapsed_seconds }`.
A projection consumer ([ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md))
— the native mobile client, phux-web — renders the waiting prompt and raises a
notification without re-deriving "is this pane waiting on me?" from the grid.
It is a sibling of the [ADR-0033](./0033-input-authority-and-process-signals.md)
`TerminalControl` broadcast: a structured agent-surface signal, not a new wire
tier. The body is field-tagged TLV so `suggestions` and `elapsed_seconds` are
additive; an older decoder skips it by length to `AgentEvent::Unknown`.

Status: Accepted
Date: 2026-06-17

## Context

"An agent is waiting on you" is the headline of a phux projection consumer:
the mobile client wants to badge the pane, surface the question, and fire a
native notification the moment an agent (Claude Code, Codex, …) blocks for
input. But the wire carries opaque VT bytes ([ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md));
every consumer would otherwise have to scrape its local grid projection and
re-derive the waiting state with per-agent heuristics — duplicated in every
consumer, and invisible to a consumer that has not yet drawn the pane.

The agent-event stream already carries structured, non-normative convenience
signals (`Bell`, `TitleChanged`, `Idle`, and `TerminalControl`). A pending
question is the same shape of signal: out-of-band, server-observed, useful to
every consumer, and not part of the grid.

## Decision

Add an additive variant to the `AgentEvent` tagged union on the `EVENT`
(`0xB3`) stream:

```
Asked { id: str, question: str, suggestions: [str], elapsed_seconds: opt<u64> }
```

at event tag `0x09` (appended after `TerminalControl`'s `0x08`). Unlike the
other (positional) `AgentEvent` bodies, the `Asked` body is itself
**field-tagged TLV** (`id`=1, `question`=2, `suggestion`=3 repeated,
`elapsed_seconds`=4), so the suggestion list and the elapsed counter are
additive and a future field is too. It mirrors the consumer-side question
model one-for-one (`AgentQuestion`).

This is a draft-level, additive change: no tag is renumbered, no existing
bytes change, and `PROTOCOL_VERSION` stays `0.5.0` (CHANGELOG `0.5.0-draft.8`).

**v1 trigger — the `phux-ask` title sentinel.** The server decides a pane is
asking by observing its terminal title (OSC 0 / OSC 2): an agent sets the
title to `phux-ask[<id>]:<question>?s=opt1|opt2`. libghostty-vt does not
surface OSC 9 / OSC 777 desktop-notification escapes through its Rust API
(title, cwd, and bell are the only user-notification signals it exposes), so
the title is the honest closest signal an agent can drive and the server can
observe without disturbing the per-consumer snapshot synthesizer. The marker
is parsed and coalesced (a re-asserted identical marker does not re-fire), and
retitling away clears the ask.

## Alternatives considered

- **Reuse the spec-only `TERMINAL_EVENT` (`0xB1`) / `USER_NOTIFICATION`.**
  That frame and OSC 9/777 plumbing are unimplemented, and OSC 9 is a generic
  desktop notification, not a structured question with suggestions. More to
  build, weaker payload.
- **`RUN_HOOK` / a hook-dispatch command.** Solves a different problem
  (running a consumer-side action), and is also spec-only.
- **No wire signal; each consumer scrapes its grid.** Duplicates per-agent
  heuristics in every consumer and cannot signal a pane a consumer has not
  drawn. Rejected — this is exactly what the agent-event stream exists to
  avoid.

## Consequences

- Forward-compatible: an older decoder skips tag `0x09` to
  `AgentEvent::Unknown { tag, body }` by the outer length prefix; consumers
  re-pin (`PHUX_REV`) rev-for-rev when they adopt it.
- The `phux-ask` title sentinel is a **v1** trigger. Full agent-state
  detection — per-agent manifests and/or opt-in agent hooks, and surfacing
  OSC 9 if libghostty grows the accessor — is follow-up work (phux-2sl6).
- `Asked` is a server-observed convenience projection, not a normative type
  system: it stays out of the L1 substrate's closed job list
  ([ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md)).
