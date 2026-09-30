---
audience: contributors
stability: stable
last-reviewed: 2026-09-30
---

# 0143 — Ship 10 MiB of scrollback per pane

**TL;DR.** `defaults.history-bytes` ships at 10 MiB, herdr's depth, up from
2 MiB. Attach already leases history instead of encoding it
([ADR-0119](./0119-attach-leases-retained-history.md)), and measurement shows
the remaining cost is paid only by panes that fill it: an idle pane is
unchanged, a flooded pane holds about 10.5 MB instead of 2.5 MB. Bootstrap
bytes and attach time are flat.

Status: Accepted
Date: 2026-09-30

## Context

ADR-0119 removed the attach argument against depth and kept the 2 MiB default
until pane-count RSS was priced. At 2 MiB a 200-column pane keeps about 1,030
rows, which a single build log or test run overflows. Measured on an isolated
release server (`phux server`, `/bin/sh` panes resized to 200x50, each flooded
with `seq 1 1500000`, about 11 MB of output; RSS from `ps`, bootstrap from
`GET_PERF` `wire.bytes_out` across `phux snapshot --rendered`, which runs the
same session kernel and history prefetch as a TUI attach):

| | 2 MiB | 10 MiB |
|---|---|---|
| RSS per idle pane (16 panes) | 1.4 MB | 1.4 MB |
| RSS per flooded pane (16 panes) | 2.4 MB | 10.5 MB |
| Rows kept at 200 columns | 1,030 | 5,790 |
| Bootstrap bytes per attach, flooded pane | 18.4 KB | 18.9 KB |
| Attach wall time incl. client start (p50 of 15) | 24 ms | 24 ms |
| Graceful upgrade, 4 flooded panes, until served | 329 ms | 449 ms |

Memory is page-granular and allocated as output arrives, so the ceiling is a
bound, not a reservation. The client pulls one page at attach and more only as
the viewport nears the loaded edge, so history depth never reaches the
bootstrap.

## Decision

1. **`DEFAULT_HISTORY_BYTES` is 10 MiB.** The line limit stays 50000 and the
   64 MiB maximum stays; only the shipped default moves.
2. **No wire change.** Leased `HISTORY_PAGE` records are unchanged; nothing
   is negotiated.
3. **The docs price it as memory per busy pane.** `default.toml`, the
   settings catalogue, and the generated reference carry the new default, the
   rows it buys, and the retain-on-exit worst case.

## Why

The number an operator feels is depth, and 2 MiB was chosen when depth cost
attach latency. That cost is gone, and the memory cost lands only on panes
that produce the output that needs scrolling back through. herdr ships 10 MB
per pane; matching it removes a visible regression against it at no attach or
idle cost.

## Tradeoffs

- A server whose panes all flood holds about 10.5 MB each instead of 2.5 MB:
  16 such panes are about 130 MB more. Lowering `history-bytes` restores the
  old bound.
- Retain on exit (ADR-0124) keeps a pane's history until it is purged, so the
  default 256 retained panes can in the worst case hold about 2.5 GiB instead
  of 512 MiB. Retention is opt-in, and `retain-on-exit-max` bounds it.
- Graceful upgrade re-encodes every pane's full history into the handoff blob,
  so its duration grows with retained depth: about 30 ms more per flooded pane
  at 10 MiB. Scrollback survives, which is the property that matters.
- The allocator keeps high-water pages after history is freed (ADR-0094), so
  a pane that once flooded keeps its peak RSS for the life of the process.

## Alternatives

**Keep 2 MiB.** Cheapest, but depth is what users compare, and the lease was
built to allow this.

**Go further, to 32 MiB.** Triples the busy-pane bill for depth few sessions
use; the knob remains for those who want it.

**Trim history on idle panes.** Would bound a flooded idle pane, but drops the
scrollback the pane exists to keep. Not built.
