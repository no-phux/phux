//! Server performance telemetry for `GET_PERF`: the host's hops, plus the
//! rows of every app it serves.
//!
//! Metric names are diagnostic, not a wire contract; see
//! `docs/troubleshooting.md#lag`.

use std::sync::OnceLock;
use std::time::Instant;

use phux_perf::{Counter, Gauge, Histogram, Metric, PerfReport, Unit};

// --- pump: raw broadcast fanout -------------------------------------------

/// `RESOURCE_OUTPUT` frames forwarded by broadcast pumps.
pub static PUMP_FRAMES: Counter = Counter::new();
/// Payload bytes those frames carried.
pub static PUMP_BYTES: Counter = Counter::new();
/// Payload bytes per forwarded frame.
pub static PUMP_FRAME_BYTES: Histogram = Histogram::new();
/// Broadcast receivers that lagged past the channel capacity and lost
/// frames. Each one costs the client a full resync.
pub static PUMP_LAGGED: Counter = Counter::new();
/// In-band resyncs requested after a lag.
pub static PUMP_GAP_RESYNC: Counter = Counter::new();

// --- wire: socket writes --------------------------------------------------

/// Microseconds per coalesced socket write (write plus flush) to a client.
pub static WIRE_WRITE: Histogram = Histogram::new();
/// Bytes per coalesced socket write.
pub static WIRE_WRITE_BYTES: Histogram = Histogram::new();
/// Total bytes written to every client.
pub static WIRE_BYTES_OUT: Counter = Counter::new();
/// Microseconds encoding one admitted outbound frame, including negotiated
/// compression. Excludes queue wait, generation-fenced drops and socket writes.
pub static WIRE_ENCODE: Histogram = Histogram::new();
/// Frames encoded per coalesced batch, even if later revoked or write-failed.
pub static WIRE_BATCH_FRAMES: Histogram = Histogram::new();

// --- control plane --------------------------------------------------------

/// Microseconds per L2 `COMMAND` handled, all kinds.
pub static CMD_HANDLE: Histogram = Histogram::new();
/// Microseconds per session `ATTACH` handled.
pub static ATTACH_HANDLE: Histogram = Histogram::new();
/// Wall microseconds staging all panes for a non-deferred attach.
///
/// Includes consumer registration, actor queues/capture replies, capability
/// adaptation and failed staging; excludes prepare, rollback and publication.
pub static ATTACH_CAPTURE_WALL: Histogram = Histogram::new();
/// Wall microseconds queueing `ATTACHED` through `ATTACH_READY`.
///
/// Includes mailbox backpressure, synchronous hook enqueue and closed-mailbox
/// failures; excludes socket drain and native publication activation.
pub static ATTACH_PUBLISH_WALL: Histogram = Histogram::new();

// --- gauges, refreshed when a report is taken -----------------------------

/// Connected clients.
pub static CLIENTS: Gauge = Gauge::new();
/// Live panes.
pub static PANES: Gauge = Gauge::new();
/// Sessions.
pub static SESSIONS: Gauge = Gauge::new();
/// `1` when the runtime thread got user-interactive scheduling.
pub static SCHED_INTERACTIVE: Gauge = Gauge::new();

/// The host's rows of the `GET_PERF` report, in render order.
pub static TABLE: &[Metric] = &[
    Metric::counter("pump.frames", Unit::Count, &PUMP_FRAMES),
    Metric::counter("pump.bytes", Unit::Bytes, &PUMP_BYTES),
    Metric::histogram("pump.frame.bytes", Unit::Bytes, &PUMP_FRAME_BYTES),
    Metric::counter("pump.lagged", Unit::Count, &PUMP_LAGGED),
    Metric::counter("pump.gap_resync", Unit::Count, &PUMP_GAP_RESYNC),
    Metric::histogram("wire.write", Unit::Micros, &WIRE_WRITE),
    Metric::histogram("wire.write.bytes", Unit::Bytes, &WIRE_WRITE_BYTES),
    Metric::counter("wire.bytes_out", Unit::Bytes, &WIRE_BYTES_OUT),
    Metric::histogram("wire.encode", Unit::Micros, &WIRE_ENCODE),
    Metric::histogram("wire.batch.frames", Unit::Count, &WIRE_BATCH_FRAMES),
    Metric::histogram("cmd.handle", Unit::Micros, &CMD_HANDLE),
    Metric::histogram("attach.handle", Unit::Micros, &ATTACH_HANDLE),
    Metric::histogram("attach.capture_wall", Unit::Micros, &ATTACH_CAPTURE_WALL),
    Metric::histogram("attach.publish_wall", Unit::Micros, &ATTACH_PUBLISH_WALL),
    Metric::gauge("proc.clients", Unit::Count, &CLIENTS),
    Metric::gauge("proc.panes", Unit::Count, &PANES),
    Metric::gauge("proc.sessions", Unit::Count, &SESSIONS),
    Metric::gauge("proc.sched_interactive", Unit::Count, &SCHED_INTERACTIVE),
];

/// Every table a report covers: each app's rows, then the host's.
pub(crate) fn tables() -> Vec<&'static [Metric]> {
    let mut tables = crate::runtime::apps::metric_tables();
    tables.push(TABLE);
    tables
}

/// Uptime epoch and the rusage read with it (the epoch restarts on upgrade
/// re-exec while the pid's rusage survives).
fn started() -> &'static (Instant, Option<phux_perf::ProcessStats>) {
    static STARTED: OnceLock<(Instant, Option<phux_perf::ProcessStats>)> = OnceLock::new();
    STARTED.get_or_init(|| (Instant::now(), phux_perf::ProcessStats::capture()))
}

/// Pin the uptime epoch and promote the runtime thread to interactive
/// scheduling (idempotent).
pub fn mark_started() {
    let _ = started();
    SCHED_INTERACTIVE.set(u64::from(phux_perf::promote_current_thread()));
}

/// Take a report. The gauges are the caller's to refresh first (they are
/// derived from registry state this module cannot see).
#[must_use]
pub fn report() -> PerfReport {
    let (epoch, baseline) = started();
    phux_perf::snapshot_since("server", &tables(), epoch.elapsed(), baseline.as_ref())
}

/// Zero every metric after a `GET_PERF { reset: true }`.
pub fn reset() {
    for table in tables() {
        phux_perf::reset(table);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_names_are_unique_and_dotted() {
        let mut names: Vec<&str> = tables()
            .into_iter()
            .flatten()
            .map(|metric| metric.name)
            .collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n, "duplicate metric name across the tables");
        assert!(names.iter().all(|name| name.contains('.')));
    }

    #[test]
    fn report_carries_every_table_row_and_a_server_role() {
        mark_started();
        let r = report();
        assert_eq!(r.role, "server");
        assert_eq!(
            r.metrics.len(),
            tables().iter().map(|table| table.len()).sum::<usize>()
        );
        assert!(r.process.is_some());
    }
}
