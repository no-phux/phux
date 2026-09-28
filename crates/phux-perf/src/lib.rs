//! Always-on in-process performance telemetry, plus the thread scheduling
//! policy for the interactive path ([`promote_current_thread`]).
//!
//! Metrics are `static` [`Histogram`]s, [`Counter`]s, and [`Gauge`]s built
//! from relaxed atomics: recording never locks, allocates, or formats. A
//! crate lists them in a `&'static [Metric]` table; [`snapshot`] walks it
//! into a [`PerfReport`] (JSON for `GET_PERF`), and [`PerfReport::delta`]
//! folds two reports into an interval for `phux perf --watch`. See
//! `docs/operations.md` §"Performance observability" for the catalog.

// `deny`, not `forbid`: the one `unsafe` in this crate is the pthread QoS
// call in `sched`, scoped by an `allow` with a `SAFETY` note.
#![deny(unsafe_code)]

mod counter;
mod histogram;
mod process;
mod render;
mod report;
mod sched;
mod throttle;

pub use counter::{Counter, Gauge};
pub use histogram::{Histogram, HistogramSnapshot, Timer};
pub use process::ProcessStats;
pub use render::render_report;
pub use report::{
    Metric, MetricSnapshot, MetricSource, MetricValue, PerfReport, SCHEMA_VERSION, Unit,
};
pub use sched::promote_current_thread;
pub use throttle::Throttle;

/// Snapshot every metric in `table` into a report tagged with `role`.
///
/// `uptime` is the caller's process uptime; the process section is captured
/// here with `getrusage(2)` and is `None` only when that syscall fails.
#[must_use]
pub fn snapshot(role: &str, table: &[Metric], uptime: std::time::Duration) -> PerfReport {
    snapshot_since(role, table, uptime, None)
}

/// [`snapshot`] with the process section taken relative to `baseline`, the
/// `getrusage` reading captured when `uptime` started counting.
///
/// A server that re-execs in place (`phux upgrade`) keeps its cumulative
/// rusage while its uptime restarts, so CPU must be measured from the
/// baseline.
#[must_use]
pub fn snapshot_since(
    role: &str,
    table: &[Metric],
    uptime: std::time::Duration,
    baseline: Option<&ProcessStats>,
) -> PerfReport {
    let process = ProcessStats::capture().map(|now| baseline.map_or(now, |base| now.delta(base)));
    PerfReport {
        stream_diagnostics: None,
        schema_version: SCHEMA_VERSION,
        role: role.to_owned(),
        pid: std::process::id(),
        captured_unix_ms: unix_ms_now(),
        uptime_ms: duration_ms(uptime),
        process,
        metrics: table.iter().map(Metric::snapshot).collect(),
    }
}

/// Zero every histogram and counter in `table`; gauges keep their reading.
///
/// Racy by design: a sample recorded during the reset lands in either the
/// old or the new epoch, which is fine for a diagnostic counter and is why
/// nothing here takes a lock.
pub fn reset(table: &[Metric]) {
    for metric in table {
        metric.reset();
    }
}

/// Wall-clock milliseconds since the Unix epoch; `0` without a wall clock
/// (wasm).
#[cfg(target_arch = "wasm32")]
const fn unix_ms_now() -> u64 {
    0
}

/// Wall-clock milliseconds since the Unix epoch; `0` if the clock is before it.
#[cfg(not(target_arch = "wasm32"))]
fn unix_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, duration_ms)
}

/// Saturating `Duration -> u64` milliseconds.
#[must_use]
pub fn duration_ms(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Saturating `Duration -> u64` microseconds, the latency histogram unit.
fn duration_us(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}
