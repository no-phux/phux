//! Server-side performance telemetry: the metric table `GET_PERF` reports.
//!
//! Every metric is a `static` from [`phux_perf`], recorded at the hop it
//! measures and read only when a client asks. Names are dotted and grouped
//! by stage so `phux perf` renders them as a pipeline you can read top to
//! bottom: `pty.*` (child output arriving), `echo.*` (input to first output
//! on the same pane), `input.*`, `tick.*` (state-sync fanout), `pump.*`
//! (raw broadcast fanout), `wire.*` (socket writes), `cmd.*` / `attach.*`
//! (control plane), `consumer.*` (per-client backpressure), and process
//! gauges. `docs/operations.md` §"Performance observability" is the human
//! catalog; the names are diagnostic and not a wire contract.

use std::sync::OnceLock;
use std::time::Instant;

use phux_perf::{Counter, Gauge, Histogram, Metric, PerfReport, Unit};

// --- pty: child output into the actor -------------------------------------

/// Bytes handed back by each `read(2)` on a PTY master. On macOS this caps at
/// 1024 regardless of buffer size, so a burst shows up here as a spike of
/// exactly-1024 reads; the histogram is how you see that.
pub static PTY_READ_SIZE: Histogram = Histogram::new();
/// Total bytes read from every PTY.
pub static PTY_READ_BYTES: Counter = Counter::new();
/// Times the reader thread found the actor's queue full and had to park in
/// `blocking_send`. Non-zero means the actor is falling behind the child.
pub static PTY_READER_BLOCKED: Counter = Counter::new();
/// Microseconds a chunk waited in the reader-to-actor queue before the actor
/// picked it up (measured on the first chunk of each burst).
pub static PTY_QUEUE_WAIT: Histogram = Histogram::new();
/// Bytes per coalesced burst handed to libghostty and broadcast as one frame.
pub static PTY_BURST_BYTES: Histogram = Histogram::new();
/// Reader chunks folded into each burst.
pub static PTY_BURST_CHUNKS: Histogram = Histogram::new();
/// Microseconds libghostty took to parse one burst.
pub static PTY_VT_APPLY: Histogram = Histogram::new();

// --- echo: input in, output out, same pane --------------------------------

/// Microseconds from handing input bytes to the PTY writer until the next
/// output burst arrived from that pane.
///
/// Includes the child's own reaction time, so it is an upper bound on the
/// server's share; samples over two seconds are discarded as "the program
/// did not echo".
pub static ECHO_SERVER: Histogram = Histogram::new();

// --- input ----------------------------------------------------------------

/// Input requests queued to a PTY writer.
pub static INPUT_EVENTS: Counter = Counter::new();
/// Microseconds for the writer thread's `write(2)` plus flush of one request.
pub static INPUT_PTY_WRITE: Histogram = Histogram::new();

// --- tick: state-sync fanout ----------------------------------------------

/// Microseconds per productive state-sync tick (grid render plus every
/// consumer diff).
pub static TICK_EMIT: Histogram = Histogram::new();
/// Microseconds per per-consumer synthesis inside a tick.
pub static TICK_SYNTH: Histogram = Histogram::new();
/// Bytes shipped per per-consumer state-sync frame.
pub static TICK_OUT_BYTES: Histogram = Histogram::new();

// --- consumer: per-client backpressure ------------------------------------

/// Ticks that skipped a consumer because its outbound mailbox was full. A
/// steady rate here is a client that cannot drain what it is sent.
pub static CONSUMER_MAILBOX_FULL: Counter = Counter::new();
/// Consumers reaped because their mailbox closed without a detach.
pub static CONSUMER_REAPED: Counter = Counter::new();
/// Microseconds from emitting a frame to receiving its `FRAME_ACK`: the
/// round trip to each state-sync client, transport included.
pub static CONSUMER_ACK_RTT: Histogram = Histogram::new();

// --- pump: raw broadcast fanout -------------------------------------------

/// `TERMINAL_OUTPUT` frames forwarded by broadcast pumps.
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

// --- control plane --------------------------------------------------------

/// Microseconds per L2 `COMMAND` handled, all kinds.
pub static CMD_HANDLE: Histogram = Histogram::new();
/// Microseconds per session `ATTACH` handled.
pub static ATTACH_HANDLE: Histogram = Histogram::new();

// --- gauges, refreshed when a report is taken -----------------------------

/// Connected clients.
pub static CLIENTS: Gauge = Gauge::new();
/// Live panes.
pub static PANES: Gauge = Gauge::new();
/// Sessions.
pub static SESSIONS: Gauge = Gauge::new();

/// The table `GET_PERF` reports, in render order.
pub static TABLE: &[Metric] = &[
    Metric::histogram("pty.read.size", Unit::Bytes, &PTY_READ_SIZE),
    Metric::counter("pty.read.bytes", Unit::Bytes, &PTY_READ_BYTES),
    Metric::counter("pty.reader.blocked", Unit::Count, &PTY_READER_BLOCKED),
    Metric::histogram("pty.queue_wait", Unit::Micros, &PTY_QUEUE_WAIT),
    Metric::histogram("pty.burst.bytes", Unit::Bytes, &PTY_BURST_BYTES),
    Metric::histogram("pty.burst.chunks", Unit::Count, &PTY_BURST_CHUNKS),
    Metric::histogram("pty.vt_apply", Unit::Micros, &PTY_VT_APPLY),
    Metric::histogram("echo.server", Unit::Micros, &ECHO_SERVER),
    Metric::counter("input.events", Unit::Count, &INPUT_EVENTS),
    Metric::histogram("input.pty_write", Unit::Micros, &INPUT_PTY_WRITE),
    Metric::histogram("tick.emit", Unit::Micros, &TICK_EMIT),
    Metric::histogram("tick.synth", Unit::Micros, &TICK_SYNTH),
    Metric::histogram("tick.out_bytes", Unit::Bytes, &TICK_OUT_BYTES),
    Metric::counter("consumer.mailbox_full", Unit::Count, &CONSUMER_MAILBOX_FULL),
    Metric::counter("consumer.reaped", Unit::Count, &CONSUMER_REAPED),
    Metric::histogram("consumer.ack_rtt", Unit::Micros, &CONSUMER_ACK_RTT),
    Metric::counter("pump.frames", Unit::Count, &PUMP_FRAMES),
    Metric::counter("pump.bytes", Unit::Bytes, &PUMP_BYTES),
    Metric::histogram("pump.frame.bytes", Unit::Bytes, &PUMP_FRAME_BYTES),
    Metric::counter("pump.lagged", Unit::Count, &PUMP_LAGGED),
    Metric::counter("pump.gap_resync", Unit::Count, &PUMP_GAP_RESYNC),
    Metric::histogram("wire.write", Unit::Micros, &WIRE_WRITE),
    Metric::histogram("wire.write.bytes", Unit::Bytes, &WIRE_WRITE_BYTES),
    Metric::counter("wire.bytes_out", Unit::Bytes, &WIRE_BYTES_OUT),
    Metric::histogram("cmd.handle", Unit::Micros, &CMD_HANDLE),
    Metric::histogram("attach.handle", Unit::Micros, &ATTACH_HANDLE),
    Metric::gauge("proc.clients", Unit::Count, &CLIENTS),
    Metric::gauge("proc.panes", Unit::Count, &PANES),
    Metric::gauge("proc.sessions", Unit::Count, &SESSIONS),
];

/// Server-side echo samples longer than this are a program that did not
/// echo, not a slow server, and are dropped rather than skewing the tail.
pub const ECHO_SAMPLE_CEILING: std::time::Duration = std::time::Duration::from_secs(2);

/// Rate limit shared by the degradation warnings this module owns.
pub const WARN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Warn throttle for a consumer whose mailbox is full.
pub static MAILBOX_FULL_WARN: phux_perf::Throttle = phux_perf::Throttle::new(WARN_INTERVAL);

fn started() -> Instant {
    static STARTED: OnceLock<Instant> = OnceLock::new();
    *STARTED.get_or_init(Instant::now)
}

/// Pin the uptime epoch. Call once at server start; harmless to call again.
pub fn mark_started() {
    let _ = started();
}

/// Take a report. The gauges are the caller's to refresh first (they are
/// derived from registry state this module cannot see).
#[must_use]
pub fn report() -> PerfReport {
    phux_perf::snapshot("server", TABLE, started().elapsed())
}

/// Zero every metric after a `GET_PERF { reset: true }`.
pub fn reset() {
    phux_perf::reset(TABLE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_names_are_unique_and_dotted() {
        let mut names: Vec<&str> = TABLE.iter().map(|m| m.name).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n, "duplicate metric name in TABLE");
        assert!(TABLE.iter().all(|m| m.name.contains('.')));
    }

    #[test]
    fn report_carries_every_table_row_and_a_server_role() {
        mark_started();
        let r = report();
        assert_eq!(r.role, "server");
        assert_eq!(r.metrics.len(), TABLE.len());
        assert!(r.process.is_some());
    }
}
