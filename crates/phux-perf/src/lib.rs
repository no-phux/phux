//! In-process performance telemetry for phux.
//!
//! Every metric here is a plain `static`: a [`Histogram`], [`Counter`], or
//! [`Gauge`] built from relaxed atomics, so recording a sample on the PTY
//! read thread, the actor's `select!` arm, or the client's paint loop costs
//! one `fetch_add` and never takes a lock, allocates, or formats anything.
//! The telemetry is therefore always on; there is no "enable profiling"
//! switch to remember, and the numbers a user reports from a laggy session
//! are the numbers the binary was already keeping.
//!
//! Reading is the expensive side and happens only on demand: a crate lists
//! its metrics as a `&'static [Metric]` table and [`snapshot`] walks it into
//! a [`PerfReport`], which serialises to JSON for the `GET_PERF` wire
//! command and for log lines. Two reports taken at different times fold
//! into an interval with [`PerfReport::delta`], which is how `phux perf
//! --watch` shows rates and per-interval percentiles rather than lifetime
//! averages that hide a stall.
//!
//! The histogram is a fixed 504-bucket log-linear layout (exact below 32,
//! then eight sub-buckets per octave up to `u64::MAX`), so a percentile is
//! accurate to within one eighth of an octave, about nine percent, which is
//! more than enough to tell a 700 µs echo from a 17 ms one. See
//! `docs/operations.md` §"Performance observability" for the metric catalog
//! and what each number should look like on a healthy machine.

#![forbid(unsafe_code)]

mod counter;
mod histogram;
mod process;
mod render;
mod report;
mod throttle;

pub use counter::{Counter, Gauge};
pub use histogram::{BUCKETS, Histogram, HistogramSnapshot};
pub use process::ProcessStats;
pub use render::render_report;
pub use report::{
    Metric, MetricSnapshot, MetricSource, MetricValue, PerfReport, SCHEMA_VERSION, Unit,
};
pub use throttle::Throttle;

/// Snapshot every metric in `table` into a report tagged with `role`.
///
/// `uptime` is the caller's process uptime; the process section is captured
/// here with `getrusage(2)` and is `None` only when that syscall fails.
#[must_use]
pub fn snapshot(role: &str, table: &[Metric], uptime: std::time::Duration) -> PerfReport {
    PerfReport {
        schema_version: SCHEMA_VERSION,
        role: role.to_owned(),
        pid: std::process::id(),
        captured_unix_ms: unix_ms_now(),
        uptime_ms: duration_ms(uptime),
        process: ProcessStats::capture(),
        metrics: table.iter().map(Metric::snapshot).collect(),
    }
}

/// Zero every metric in `table`. Racy by design: a sample recorded during
/// the reset lands in either the old or the new epoch, which is fine for a
/// diagnostic counter and is why nothing here takes a lock.
pub fn reset(table: &[Metric]) {
    for metric in table {
        metric.reset();
    }
}

/// Wall-clock milliseconds since the Unix epoch; `0` if the clock is before it.
#[must_use]
pub fn unix_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, duration_ms)
}

/// Saturating `Duration -> u64` milliseconds.
#[must_use]
pub fn duration_ms(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Saturating `Duration -> u64` microseconds, the unit every latency
/// histogram in phux records.
#[must_use]
pub fn duration_us(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}
