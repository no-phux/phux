---
audience: contributors
stability: stable
last-reviewed: 2026-09-13
---

# 0119 — Attach leases retained history instead of encoding it

**TL;DR.** Releasing a READY capture detaches an O(1) engine history cut and
encodes nothing. Each `HISTORY_PAGE` is encoded from the live page when its
`HISTORY_REQUEST` arrives. `defaults.history-bytes` stays 2 MiB; the 64 MiB
maximum is a resident-memory bound, not an attach-latency one.

Status: Accepted
Date: 2026-09-13

## Context

[ADR-0094](./0094-explicit-per-pane-scrollback-byte-ceiling.md) priced
`defaults.history-bytes` as attach latency because releasing READY encoded
every retained page inside the terminal's mutation exclusion, on the single
server thread, once per pane per attach: 8 ms at 2 MiB, 65 ms at 10 MiB,
222 ms at 32 MiB. Raising the shipped default toward herdr's 10 MiB was gated
on leasing history instead. The official GHOSTSNP capture now detaches at
READY with O(1) storage. Docs and the schema still described the old cost.

## Decision

1. **Attach leases.** `OwnedCapture::detach` registers an engine cut (tracked
   pins and the history generation) and copies no page. `history_record_at`
   encodes one record per `HISTORY_REQUEST`, one actor turn, from the live
   page after the engine re-validates the cut.
2. **The shipped default stays 2 MiB.** Attach no longer argues against depth;
   resident memory per pane still does. A 10 MiB bump is a follow-up.
3. **`MAX_HISTORY_BYTES` (64 MiB) is a memory bound.** `phux config check`
   still rejects more. A dozen panes at the ceiling is most of a gigabyte.
4. **A live lease can lose pages.** Prune, mutation, reset, and resize are
   typed engine invalidations mapped to `HISTORY_TOMBSTONE`. A frontier
   failure is kept so every owner of the generation gets that reason, not
   `InvalidHandle` / `CodecFailure`.

No wire or protocol change: leased records are byte-identical in shape.

## Why

`docs/spec/L1.md` §4.4 already describes history as a bounded lease that
must not block live writes or aggregate attach readiness. Encoding at READY
was the implementation that had not caught up, and it made the advertised
ceiling unusable. The engine's detached cut is that lease. Memory is still
the operator's choice, which is why the default does not move.

## Tradeoffs

- Laziness moves work: a client that scrolls back pays to encode each page
  it asks for, bounded per turn instead of per attach.
- Live output can prune history a client has not fetched yet. Once a prune
  reaches the oldest leased page, that replica's stream ends with `Pruned`.
  The eager path could not lose it, because it encoded everything while the
  terminal was frozen. The attach itself stays alive.
- A lease is row-exact where the eager walk exported whole pages, so a
  `HISTORY_PAGE` can be larger. A client whose page limit cannot hold one
  gets the existing `TooSmall` rejection.

## Alternatives

**Raise the default to 10 MiB with the lease.** Attach no longer argues
against it, but the memory bill is per pane for the life of the session.
Follow-up once operators have priced pane-count RSS.

**Keep encoding at READY.** Rejected: it is the cost ADR-0094 measured and
the reason 10 MiB was gated.

**New wire.** Unneeded; leased records are the same bytes the client already
pulls one page at a time.
