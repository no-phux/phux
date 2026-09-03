//! Client-side performance telemetry: what the attach loop keeps about its
//! own responsiveness.
//!
//! The one number a user feels is `echo.rtt`: the time from a keystroke
//! leaving this process until the first `TERMINAL_OUTPUT` for the pane it
//! went to arrives back. It is sampled where the paint pacer already
//! observes replies, so it costs nothing new. Around it sit the paint-side
//! costs (`vt_apply`, `paint.*`, `stdout.*`) and the pacer's own decisions
//! (`pacer.*`), so a slow session can be read as "the server was slow" or
//! "we were slow to draw it" from the same table.
//!
//! Every metric is a `static` from [`phux_perf`] and is always on. The
//! render-profile log line (`PHUX_RENDER_PROF=1`) and the one-line summary
//! written to the client log when an attach ends are both rendered from this
//! table; `render_prof` is the compatibility shim that feeds it.

use std::sync::OnceLock;
use std::time::Instant;

use phux_perf::{Counter, Histogram, Metric, PerfReport, Unit};

/// Microseconds from sending input for a pane to the first output frame
/// from that pane.
pub static ECHO_RTT: Histogram = Histogram::new();
/// Microseconds libghostty took to apply one `TERMINAL_OUTPUT` frame.
pub static VT_APPLY: Histogram = Histogram::new();
/// Microseconds per full-frame paint (every pane, chrome, flush).
pub static PAINT_FULL: Histogram = Histogram::new();
/// Microseconds per chrome-only paint.
pub static PAINT_CHROME: Histogram = Histogram::new();
/// `TERMINAL_OUTPUT` frames received.
pub static FRAMES: Counter = Counter::new();
/// Frames that led to a paint.
pub static PAINTS: Counter = Counter::new();
/// Frames skipped as no-ops.
pub static SKIPPED: Counter = Counter::new();
/// Status-bar compositions.
pub static BAR_COMPOSES: Counter = Counter::new();
/// Layout computations.
pub static LAYOUTS: Counter = Counter::new();
/// Stdout flushes.
pub static FLUSHES: Counter = Counter::new();
/// Bytes written to the outer terminal.
pub static BYTES_OUT: Counter = Counter::new();
/// Times the stdout backlog crossed its cap and queued diffs were dropped
/// for a resync. Any non-zero value means the outer terminal could not keep
/// up with what we were sending it.
pub static STDOUT_DROPS: Counter = Counter::new();
/// Frames the pacer let through immediately because they answered input.
pub static PACER_REPLIES: Counter = Counter::new();
/// Frames the pacer held for the next frame interval.
pub static PACER_WAITS: Counter = Counter::new();
/// `1` when the attach loop's thread was promoted to user-interactive
/// scheduling, `0` otherwise.
pub static SCHED_INTERACTIVE: phux_perf::Gauge = phux_perf::Gauge::new();

/// The client's metric table, in render order.
pub static TABLE: &[Metric] = &[
    Metric::histogram("echo.rtt", Unit::Micros, &ECHO_RTT),
    Metric::histogram("vt_apply", Unit::Micros, &VT_APPLY),
    Metric::histogram("paint.full", Unit::Micros, &PAINT_FULL),
    Metric::histogram("paint.chrome", Unit::Micros, &PAINT_CHROME),
    Metric::counter("frames.received", Unit::Count, &FRAMES),
    Metric::counter("frames.painted", Unit::Count, &PAINTS),
    Metric::counter("frames.skipped", Unit::Count, &SKIPPED),
    Metric::counter("frames.bar_composes", Unit::Count, &BAR_COMPOSES),
    Metric::counter("frames.layouts", Unit::Count, &LAYOUTS),
    Metric::counter("stdout.flushes", Unit::Count, &FLUSHES),
    Metric::counter("stdout.bytes", Unit::Bytes, &BYTES_OUT),
    Metric::counter("stdout.drops", Unit::Count, &STDOUT_DROPS),
    Metric::counter("pacer.replies", Unit::Count, &PACER_REPLIES),
    Metric::counter("pacer.waits", Unit::Count, &PACER_WAITS),
    Metric::gauge("proc.sched_interactive", Unit::Count, &SCHED_INTERACTIVE),
];

/// Rate limit for the stdout-drop warning.
pub static STDOUT_DROP_WARN: phux_perf::Throttle =
    phux_perf::Throttle::new(std::time::Duration::from_secs(10));

fn started() -> Instant {
    static STARTED: OnceLock<Instant> = OnceLock::new();
    *STARTED.get_or_init(Instant::now)
}

/// Pin the uptime epoch and promote the calling thread (the attach loop's
/// runtime thread) to interactive scheduling; call when the attach starts.
pub fn mark_started() {
    let _ = started();
    SCHED_INTERACTIVE.set(u64::from(phux_perf::promote_current_thread()));
}

/// Snapshot the client table.
#[must_use]
pub fn report() -> PerfReport {
    phux_perf::snapshot("client", TABLE, started().elapsed())
}

/// One line for the client log when an attach ends: the echo and paint
/// percentiles a user would want to paste into a bug report.
#[must_use]
pub fn summary_line() -> String {
    let echo = ECHO_RTT.snapshot();
    let paint = PAINT_FULL.snapshot();
    let apply = VT_APPLY.snapshot();
    format!(
        "session perf: echo n={} p50={}us p99={}us max={}us; vt_apply p99={}us; paint.full n={} p50={}us p99={}us; frames={} painted={} pacer_waits={} stdout_drops={}",
        echo.count,
        echo.percentile(50),
        echo.percentile(99),
        echo.max,
        apply.percentile(99),
        paint.count,
        paint.percentile(50),
        paint.percentile(99),
        FRAMES.get(),
        PAINTS.get(),
        PACER_WAITS.get(),
        STDOUT_DROPS.get(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_names_are_unique() {
        let mut names: Vec<&str> = TABLE.iter().map(|m| m.name).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n);
    }

    #[test]
    fn summary_line_names_the_headline_numbers() {
        let line = summary_line();
        assert!(line.starts_with("session perf: echo n="), "{line}");
        assert!(line.contains("paint.full"), "{line}");
        assert!(line.contains("stdout_drops="), "{line}");
    }

    #[test]
    fn report_is_tagged_client() {
        mark_started();
        let r = report();
        assert_eq!(r.role, "client");
        assert_eq!(r.metrics.len(), TABLE.len());
    }
}
