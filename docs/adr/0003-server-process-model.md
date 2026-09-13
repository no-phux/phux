---
audience: contributors
stability: stable
last-reviewed: 2026-05-28
---

# 0003 — Single server, many sessions

**TL;DR.** One phux-server per user holds every session in a single process (tmux's model), not one daemon per session (zmx's model). Cross-session operations become pointer moves, clients attach to one socket, and the system has one scheduler and one source of truth. The crash blast radius is mitigated by testing and journaled recovery.

Status: Accepted
Date: 2026-05-24

## Context

The server-side process organization is a foundational decision:

- **Option A (zmx):** one daemon per session.
- **Option B (tmux):** one server per user; all sessions live in one
  process.

## Decision

Option B. One server per user.

## Rationale

- **Cross-session operations are first-class.** Moving a window between
  sessions, shared paste buffers, a single command surface. Across
  separate daemons these would require an additional inter-daemon
  layer; in one process they are pointer operations.
- **One IPC surface.** Clients attach to one Unix socket and address
  sessions by name. No discovery layer, no per-session socket lookup.
- **Lower system overhead.** N sessions in one process instead of N
  processes.
- **One scheduler, one event loop, one source of truth.** Easier to
  reason about, easier to debug, easier to test.

## Tradeoffs

- **Larger crash blast radius.** A bug in the server takes down every
  session. We mitigate by:
  - Aggressive testing (proptest, insta, mutation testing).
  - Journaling PTY output to disk; `--recover` mode reconstitutes
    state from journals after an unexpected exit.
  - Per-client outbound queue isolation so a wedged client cannot
    block the server (`SPEC.md` §12.3).
- **Memory grows with session count.** Acceptable. tmux has lived
  with this for 17 years on far less generous machines than today's.

## Alternatives considered

- **One daemon per session (zmx).** Isolation is real but the UX cost
  is high: cross-session operations become difficult, the user must
  reason about per-session daemons, and the IPC surface multiplies.
  Isolation is solvable via testing and recovery; the UX of one-
  daemon-per-session is harder to fix.
- **No daemon at all, peer-to-peer between clients.** Considered
  briefly. Rejected: session persistence is the entire point of a
  multiplexer; no daemon means no detach.
