//! Client-side performance telemetry for the attach loop.
//!
//! `echo.rtt` (a keystroke to its pane's first output frame), the paint-side
//! costs, and the pacer's decisions, so "the server was slow" and "we drew
//! slowly" read from one always-on table.

use std::sync::OnceLock;
use std::time::Instant;

use phux_perf::{Counter, Histogram, Metric, PerfReport, Unit};

/// Microseconds from sending input for a pane to the first output frame
/// from that pane.
pub static ECHO_RTT: Histogram = Histogram::new();
/// Microseconds libghostty took to apply one `RESOURCE_OUTPUT` frame.
pub static VT_APPLY: Histogram = Histogram::new();
/// Microseconds composing a full frame into memory, excluding submission and tty I/O.
pub static PAINT_FULL: Histogram = Histogram::new();
/// Microseconds per chrome-only paint.
pub static PAINT_CHROME: Histogram = Histogram::new();
/// Microseconds per pane render attempt, including clean and failed attempts;
/// excludes letterbox margins, composite chrome, submission, and tty I/O.
pub static PAINT_PANE: Histogram = Histogram::new();
/// Microseconds updating/acquiring pooled libghostty render state.
pub static PAINT_PREPARE: Histogram = Histogram::new();
/// Microseconds walking dirty rows, front-buffer diffing and emitting VT into memory.
pub static PAINT_ROWS: Histogram = Histogram::new();
/// `RESOURCE_OUTPUT` frames received.
pub static FRAMES: Counter = Counter::new();
/// Frames that led to a paint.
pub static PAINTS: Counter = Counter::new();
/// Frames skipped as no-ops.
pub static SKIPPED: Counter = Counter::new();
/// Status-bar compositions.
pub static BAR_COMPOSES: Counter = Counter::new();
/// Layout computations.
pub static LAYOUTS: Counter = Counter::new();
/// Nonempty stdout sink flush submissions.
pub static FLUSHES: Counter = Counter::new();
/// Bytes offered to the stdout writer, including frames later dropped.
pub static BYTES_OUT: Counter = Counter::new();
/// Times the stdout backlog crossed its cap and queued diffs were dropped
/// for a resync: the outer terminal could not keep up.
pub static STDOUT_DROPS: Counter = Counter::new();
/// Microseconds copying/submitting a composited frame to the render sink.
pub static PAINT_SUBMIT: Histogram = Histogram::new();
/// Microseconds from stdin readiness through handling, including awaited sends.
pub static INPUT_WALL: Histogram = Histogram::new();
/// Microseconds from socket readiness through burst handling, including awaited sends.
pub static FRAMES_WALL: Histogram = Histogram::new();
/// Frames drained per inbound burst.
pub static BURST_FRAMES: Histogram = Histogram::new();
/// Bursts that reached the fairness cap.
pub static BURST_CAPPED: Counter = Counter::new();
/// Microseconds from first withheld pane until its debt is retired (not tty delivery).
pub static PACER_HOLD: Histogram = Histogram::new();
/// Microseconds past the pacing deadline when withheld debt is retired.
pub static PACER_LATE: Histogram = Histogram::new();
/// Panes whose withheld debt is retired together.
pub static PACER_PANES: Histogram = Histogram::new();
/// Queued bytes sampled after each nonempty sink flush; excludes in-flight writes.
pub static STDOUT_BACKLOG: Histogram = Histogram::new();
/// Microseconds from enqueue until a chunk's actual write starts.
pub static STDOUT_QUEUE_WAIT: Histogram = Histogram::new();
/// Microseconds in each actual `write_all` call, including blocking and failed calls.
pub static STDOUT_WRITE: Histogram = Histogram::new();
/// Microseconds in each actual writer-thread flush, including failed calls.
pub static STDOUT_FLUSH: Histogram = Histogram::new();
/// Bytes in successfully completed writer-thread `write_all` calls.
pub static STDOUT_WRITTEN: Counter = Counter::new();
/// Bytes discarded on backlog overflow, including the triggering frame.
pub static STDOUT_DROPPED_BYTES: Counter = Counter::new();
/// Actual writer-thread `write_all` or flush errors.
pub static STDOUT_ERRORS: Counter = Counter::new();
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
    Metric::histogram("paint.pane", Unit::Micros, &PAINT_PANE),
    Metric::histogram("paint.prepare", Unit::Micros, &PAINT_PREPARE),
    Metric::histogram("paint.rows", Unit::Micros, &PAINT_ROWS),
    Metric::histogram("paint.submit", Unit::Micros, &PAINT_SUBMIT),
    Metric::histogram("loop.input_wall", Unit::Micros, &INPUT_WALL),
    Metric::histogram("loop.frames_wall", Unit::Micros, &FRAMES_WALL),
    Metric::histogram("loop.burst_frames", Unit::Count, &BURST_FRAMES),
    Metric::counter("loop.burst_capped", Unit::Count, &BURST_CAPPED),
    Metric::counter("frames.received", Unit::Count, &FRAMES),
    Metric::counter("frames.painted", Unit::Count, &PAINTS),
    Metric::counter("frames.skipped", Unit::Count, &SKIPPED),
    Metric::counter("frames.bar_composes", Unit::Count, &BAR_COMPOSES),
    Metric::counter("frames.layouts", Unit::Count, &LAYOUTS),
    Metric::counter("stdout.flushes", Unit::Count, &FLUSHES),
    Metric::counter("stdout.bytes", Unit::Bytes, &BYTES_OUT),
    Metric::counter("stdout.drops", Unit::Count, &STDOUT_DROPS),
    Metric::histogram("stdout.backlog", Unit::Bytes, &STDOUT_BACKLOG),
    Metric::histogram("stdout.queue_wait", Unit::Micros, &STDOUT_QUEUE_WAIT),
    Metric::histogram("stdout.write", Unit::Micros, &STDOUT_WRITE),
    Metric::histogram("stdout.flush", Unit::Micros, &STDOUT_FLUSH),
    Metric::counter("stdout.written", Unit::Bytes, &STDOUT_WRITTEN),
    Metric::counter("stdout.dropped_bytes", Unit::Bytes, &STDOUT_DROPPED_BYTES),
    Metric::counter("stdout.errors", Unit::Count, &STDOUT_ERRORS),
    Metric::counter("pacer.replies", Unit::Count, &PACER_REPLIES),
    Metric::counter("pacer.waits", Unit::Count, &PACER_WAITS),
    Metric::histogram("pacer.hold", Unit::Micros, &PACER_HOLD),
    Metric::histogram("pacer.late", Unit::Micros, &PACER_LATE),
    Metric::histogram("pacer.panes", Unit::Count, &PACER_PANES),
    Metric::gauge("proc.sched_interactive", Unit::Count, &SCHED_INTERACTIVE),
];

/// Rate limit for the stdout-drop warning.
pub static STDOUT_DROP_WARN: phux_perf::Throttle =
    phux_perf::Throttle::new(std::time::Duration::from_secs(10));

/// Uptime epoch and the rusage reading taken with it, so the process section
/// covers the same span as the uptime (see the server's `perf::started`).
fn started() -> &'static (Instant, Option<phux_perf::ProcessStats>) {
    static STARTED: OnceLock<(Instant, Option<phux_perf::ProcessStats>)> = OnceLock::new();
    STARTED.get_or_init(|| (Instant::now(), phux_perf::ProcessStats::capture()))
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
    let (epoch, baseline) = started();
    phux_perf::snapshot_since("client", TABLE, epoch.elapsed(), baseline.as_ref())
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
}
