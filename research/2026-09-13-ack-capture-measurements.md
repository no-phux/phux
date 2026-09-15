---
audience: agents, contributors
stability: scratch
last-reviewed: 2026-09-13
---

# ACK capture and map-pruning measurements

**TL;DR.** Reusing the state-tick renderer for ACK metadata eliminates repeated
viewport allocations and fixes a reproduced missed-row update. In-place map
pruning eliminates the allocations made by `BTreeMap::split_off`. These are
component allocation measurements, not WAN latency or whole-server CPU results.

This is acceptance evidence for **phux-ghfu** in the
[protocol optimization audit](2026-09-12-protocol-optimization-audit.md).

## Reproduce

```sh
cargo run --locked -p phux-server --example ack_allocations --features ack-allocations-example
```

The example routes `RenderState` allocations through `Allocator::GLOBAL`, so
DHAT observes engine allocations as well as Rust tree allocations. Each cursor
capture reads viewport position, visibility, visual style, and blinking. Terminal
construction and intervening VT writes are outside the counted intervals. The
reused variant includes its first allocation; subsequent captures use the same
geometry. The production path shares the already-existing synthesizer pool.

Observed on darwin/arm64, Rust 1.98.1, libghostty-vt revision `920ce00`:

| 1,000 captures | Fresh allocations | Reused allocations | Fresh allocated bytes | Reused allocated bytes |
|---|---:|---:|---:|---:|
| 80×24 | 26,000 | 26 | 160,263,000 | 160,263 |
| 200×60 | 62,000 | 62 | 959,553,000 | 959,553 |

These byte totals are cumulative allocation traffic, not resident-memory peaks.
The sample does not extrapolate unchanged-geometry counts to resize storms.

For tree pruning, reinsertion is outside the measured interval and both variants
retain the same number of entries after every cumulative ACK:

| Window / 10,000 prunes | `split_off` allocations | In-place allocations | `split_off` allocated bytes | In-place allocated bytes |
|---|---:|---:|---:|---:|
| 1 | 10,000 | 0 | 1,920,000 | 0 |
| 8 | 10,000 | 0 | 1,920,000 | 0 |
| 32 | 20,000 | 0 | 4,800,000 | 0 |
| 256 | 30,000 | 0 | 7,680,000 | 0 |

## Correctness and cadence

The production actor regression
`ack_between_pty_mutation_and_tick_preserves_pending_row_changes` puts a second
row mutation between an emitted frame and its ACK, then runs the next state tick.
With a separate fresh ACK `RenderState`, the row was missing from the next
emitted reference. The same test passes when ACK metadata uses the synthesizer's
render cache. `RenderState::update` consumes terminal dirty flags; retaining the
updated rows in the tick's own cache is essential, independently of allocation
reuse. An initial draft asserted a loss-tolerant pending map on a reliable
consumer; that defective assertion was corrected before the before/after run.

The shared-consumer regression also passes across resize, alternate-screen
entry/exit, cursor visibility, cursor movement, and bracketed-paste changes.
The existing paused-time actor test retains the exact cadence behavior: a 400 ms
RTT sample produces a 200 ms tick; a second consumer with a 50 μs sample selects
the shared 20 ms floor; removing both returns to the cold-start default.

ACK transmission policy is unchanged: this optimization neither batches ACKs
nor reduces their wire count. Each accepted ACK can still provide its own RTT
sample. CPU timing and real-path frame counts belong in the integrated audit
measurement run rather than being inferred from these allocation totals.

## Validation

- Strict server Clippy: `cargo clippy --locked -p phux-server --all-targets --all-features -- -D warnings`.
- Server library: all **1,080 tests passed** in a serial run. The preceding
  four-thread run passed all ACK regressions but hit the existing foreground
  SIGHUP-flush fixture once; that fixture passed in isolation and in the full
  serial run without source changes.
- Independent source review identified the dirty-bit issue and the initial test
  assertion defect; both were addressed before acceptance.
- Lizard: `capture_acked_cursor_mode` complexity **5 → 3**;
  `on_frame_ack` stays **6** after the earlier **10 → 6** admission split;
  the new `metadata_snapshot` helper is **2**. The allocation example's
  measurement and sampling helpers are at most **8**.
