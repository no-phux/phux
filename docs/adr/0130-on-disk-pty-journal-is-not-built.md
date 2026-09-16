---
audience: contributors
stability: stable
last-reviewed: 2026-09-15
---

# 0130 — The on-disk PTY output journal is not built

**TL;DR.** The server keeps no durable PTY output and ships no `--recover`
mode: a crash loses every pane's scrollback, and clients reattach to a
fresh server with no memory of what ran there. The `EVENT` journal stays
memory-bounded (ADR-0123) and carries no PTY bytes; durable work state
belongs to a separate coordinator endpoint (ADR-0097) that itself carries
no terminal output either. This is a decision, not an unfinished feature:
PHA-406 phase 2's H3 closes phux-p91i as decided, not built.

Status: Accepted
Date: 2026-09-15

## Context

ADR-0003's crash-blast-radius mitigation names "journaling PTY output to
disk; `--recover` mode reconstitutes state from journals after an
unexpected exit," and ADR-0092 still calls the on-disk PTY journal "design
intent, not shipped behavior." Neither exists: the server keeps every
resource in memory only. Two later ADRs journal something and are easy to
mistake for covering this gap. ADR-0123 journals `EVENT` — kind, actor,
sequence, an attribution stamp — never `RESOURCE_OUTPUT` bytes; its ring is
sized for control-plane events, not PTY volume, and a restart empties it by
design. ADR-0097's coordinator endpoint "references Terminals without
carrying terminal output or input." Neither one is the PTY-output journal
ADR-0003 and ADR-0092 still name, and nothing since has built it or
scheduled it. PHA-406 asked whether it should.

## Decision

The server keeps no durable record of PTY output and ships no `--recover`
mode. After a server crash, every pane's process and its scrollback are
gone; clients reattach to a fresh server. `phux-server` will not journal
`RESOURCE_OUTPUT` bytes to disk. The `ADR-0123` event journal is a
different stream and stays memory-bounded; durable *work* state is the
`phux-coordinator/1` endpoint's job (ADR-0097), and that endpoint carries
no terminal output either, so it is not a substitute. A durable PTY output
history, if ever built, is a separate, explicitly designed feature — a
recorder extension (ADR-0060) or an opt-in capture sink someone chooses to
enable — not a server journal every process pays for.

## Why

The mitigation in ADR-0003 assumed the journal would eventually ship
alongside the single-process model; it has not, and no PHA-406 journey
needs it. ADR-0123 already declined durable event history in
`phux-server`; this closes the PTY-output half of the same question left
open by ADR-0003 and ADR-0092. Building a general-purpose,
crash-survivable output journal means deciding a replay format, a
corruption story, and a recovery UX that nothing evidenced here asks for;
`phux rec` already answers "I want to keep what ran here" as an opt-in,
purpose-built feature instead of a standing tax on every pane.

## Tradeoffs

A crash loses scrollback and running state for every pane, with no
`--recover` to reconstruct it. What already mitigates that: `phux rec`
records a session as it runs, independent of the server crashing later
(ADR-0060); a Terminal spawned with `retain_secs` keeps its exit status and
last grid readable after its own process exits, though not after the
server itself dies (ADR-0124); and a client that read a snapshot before the
crash still holds what it read. None of these replace a server-durable
journal — they are the answers phux gives instead of building one.

## Alternatives

**Build the journal ADR-0003 named.** Rejected: no PHA-406 journey needs
crash-survivable PTY output, and ADR-0097 already gives durable work state
a home that deliberately excludes terminal output, not by omission.

**Fold PTY output into the ADR-0123 event journal.** Rejected: that
journal is bounded and sized for control-plane events. PTY byte volume is
a different scale problem with its own replay semantics; conflating the
two would make the event journal's bounds meaningless.

---

References: phux-p91i; PHA-406 phase-2 plan §3 H3.
