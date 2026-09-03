---
audience: contributors
stability: stable
last-reviewed: 2026-09-02
---

# 0096 — Performance telemetry is always on, in-process, and one command away

**TL;DR.** Every hop on phux's hot path now records into a lock-free
histogram or counter that lives in the binary as a `static`, on both ends
of the wire, with no switch to turn on. A new session-scoped command,
`GET_PERF`, returns the server's table as JSON; `phux perf --watch` renders
it as interval deltas; the client writes one summary line to its log when an
attach ends. The primitives live in a new narrow crate, `phux-perf`, with no
dependency on tokio, tracing, or libghostty, so the server, the ratatui
client, the FFI kernel, and the CLI can all share one histogram and one
report shape.

Status: Accepted
Date: 2026-09-02

## Context

The September 2026 perf campaign (phux-l96p) landed eight optimisation
commits, measured with out-of-process probes (`scripts/bench/pty-echo.py`,
`scripts/bench/mux-compare.sh`), and the maintainer still reported a laggy
server the same evening. Nothing in the running binary could say why. The
tracing spans on the hot path are debug-level and cover only the state-sync
tick and the client paint; the whole input-to-PTY-to-broadcast-to-client leg
was untimed. The only counters (`render_prof`) were counts, not durations,
and were gated behind an env var nobody sets before a session goes bad.
Turning on the debug filter that would have produced timings puts a
synchronous file write inside the client's paint loop, so the act of
measuring changed the thing being measured. Degradation signals (a consumer
mailbox full, a stdout backlog dropped) logged at debug and were invisible at
the default filter.

The result was that every performance question had to be re-created in a
bench harness after the fact, and the numbers from the session the user was
actually complaining about were gone.

## Decision

1. **A `phux-perf` crate owns the primitives.** A 504-bucket log-linear
   `Histogram` (exact below 32, eight sub-buckets per octave, under 9 percent
   percentile error), a `Counter`, a `Gauge`, a `Throttle` for rate-limited
   warnings, `getrusage` process statistics, and a `PerfReport` that
   serialises to JSON and folds two reports into an interval. Recording is
   one relaxed `fetch_add`; nothing allocates, locks, or formats on the hot
   path. It has no runtime dependencies beyond `serde` and `nix`, both
   already in the graph, so it can sit under every crate that has a hot path.
2. **Metrics are `static`s, always on.** Each crate declares its metrics as
   statics in a `perf` module and lists them in a `&'static [Metric]` table.
   There is no enable flag: the cost of an always-on counter is below the
   noise floor of the loop it sits in, and a switch nobody remembers to set
   is the failure mode this ADR exists to remove.
3. **`GET_PERF { reset }` is the wire surface**, tag `0x18`, gated on
   `ServerFeature::GET_PERF`, answered with `CommandValue::Json`. The metric
   names inside are diagnostic and explicitly not contract (L1.md §5.1), so
   a probe can be added at a new hop without a spec change. It ships as a
   draft under the existing `0.8.0` because §6.1's `major.minor` admission
   test would otherwise lock out every deployed 0.8 client.
4. **Degradation is a rate-limited `warn!`, not a `debug!`.** A full consumer
   mailbox, a dropped stdout backlog, and a broadcast lag each warn at most
   once per interval with a suppressed count, so the log carries the fact
   and the magnitude at the default filter without carrying the volume.
5. **The client reports itself.** `echo.rtt` is sampled where the paint
   pacer already observes replies; paint and apply durations use a drop
   timer; the attach loop logs one `session perf:` line on exit. The
   `PHUX_RENDER_PROF` line survives, rendered from the same table.

## Consequences

Positive: `phux perf --watch 1` beside a laggy session shows which stage
moved, in that interval, on the binary the user is running. `pty.read.size`
makes the macOS 1024-byte PTY read cap visible as a distribution rather than
folklore. A regression in echo p99 is a number in a log line after every
session, not a bench re-run.

Negative: about 4 KiB of atomics per histogram, roughly 130 KiB per process
for the current tables, resident for the life of the process. A wire command
that returns a JSON blob is less typed than the rest of the L1 catalog; the
schema version inside the report is the mitigation. Peak RSS from
`getrusage` is a peak, not a current reading.

## Alternatives considered

- **The `metrics` crate with an exporter.** A global recorder behind trait
  objects, a dependency on an ecosystem with its own release cadence, and a
  wire format chosen by the exporter rather than by phux. The histogram
  phux needs is 150 lines.
- **Percentiles from tracing spans.** Requires the subscriber to be on and
  a file to be written; the debug filter's synchronous client-side write is
  exactly the perturbation this ADR removes.
- **A typed binary `PerfReport` frame.** Every new probe would be a spec
  change. The names are supposed to churn as bottlenecks move.
- **Keep it out-of-process (`pty-echo.py` and friends).** Those stay; they
  are the regression gate. They cannot see inside a session that already
  went wrong.
