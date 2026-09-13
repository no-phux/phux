---
audience: contributors
stability: stable
last-reviewed: 2026-05-28
---

# 0005 — Relationship to zmx and zmosh

**TL;DR.** phux is greenfield relative to zmx and zmosh: we steal patterns (PTY supervision, journaling, libghostty idioms, frozen wire shapes) and share an ecosystem, but we do not share code or fork their codebases. Their single-session, one-daemon-per-session data model does not map cleanly to phux's multi-session forest.

Status: Accepted
Date: 2026-05-24

## Context

zmx (single-session daemon with libghostty-vt state replay) and zmosh
(zmx + mosh-style UDP transport) are the existing libghostty-based
session tools. Before we commit to greenfield, we should be sure that
neither building on top nor forking is the better path.

## Decision

Greenfield. Steal patterns, share an ecosystem, do not share code.

## Rationale

zmx is deliberately scoped: one daemon per session, one PTY per
daemon, VT bytes on the wire. That shape is right for
abduco-with-state-replay — its intended product. Our shape is
multi-session, multi-window, multi-pane, with structured cell-level
diffs. The two diverge at the data model: zmx's `Daemon` owns one PTY;
ours owns a forest. No subset of zmx maps cleanly to a subset of phux.

- **Build on top** (each phux pane is a zmx daemon): process explosion
  (one OS process per pane), and we'd re-parse VT bytes from zmx's
  output channel to construct the diffs we already own server-side.
  Defeats the protocol's premise.
- **Fork**: porting ~5k lines of Zig to Rust while contorting their
  data model into ours costs more than the greenfield it produces.
- **Greenfield**: keeps the language decision (ADR-0001), keeps
  architectural unity, and lets zmx remain itself.

## Tradeoffs

We re-implement PTY supervision, journaling, signal handling, and
IPC plumbing zmx already debugged. We mitigate by adopting their
patterns rather than reinventing:

- Binary IPC with a non-exhaustive tag enum for forward-compat
  (zmx's `_` arm pattern; mirrored in our codec).
- Journaled raw output for crash recovery
  ([`docs/architecture/`](../architecture/)).
- libghostty-vt usage idioms (see zmx's `daemonLoop` and `util.zig`).
- "Frozen wire shape" discipline for stable structs (our spec
  achieves this with field-IDed encoding; see `SPEC.md` Appendix A).

We do not get zmosh's network resilience for free. Acceptable: the
protocol's transport abstraction (`SPEC.md` §4) accommodates a
resilient transport in a future minor version without protocol-level
changes.

## Going forward

| Project | Scope                                          |
|---------|------------------------------------------------|
| zmx     | single-session persistence (abduco + replay)   |
| zmosh   | zmx + UDP/SSP network resilience               |
| phux    | full multiplexer (windows, panes, structured)  |

We will credit both projects in `README.md`, watch their work, and look
for shared infrastructure to contribute upstream (e.g. a future
`libghostty-vt-diff` crate either project could opt into).
