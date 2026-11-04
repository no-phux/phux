//! The Terminal's half of the server's `GET_PERF` telemetry.
//!
//! Static [`phux_perf`] metrics are recorded at each hop, grouped by pipeline
//! stage (`pty.*`, `echo.*`, `input.*`, `tick.*`, `consumer.*`,
//! `bootstrap.*`). Names are diagnostic, not a wire contract; see
//! `docs/troubleshooting.md#lag`.

use phux_perf::{Counter, Histogram, Metric, Unit};

// --- pty: child output into the actor -------------------------------------

/// Bytes per PTY master `read(2)` (macOS caps at 1024).
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
/// Microseconds per synchronous non-deferred ingest, including parsing and
/// derived output work. Excludes native capture replay.
pub static PTY_VT_APPLY: Histogram = Histogram::new();
/// Microseconds spent only in the canonical terminal's VT parser.
pub static PTY_VT_PARSE: Histogram = Histogram::new();
/// Microseconds after parsing: color replies, input-mode publication, semantic
/// events/cwd checks and starting a queued native capture.
pub static PTY_POST_APPLY: Histogram = Histogram::new();
/// Wall microseconds to resume a cooperative yield after a byte-capped PTY
/// burst. Includes scheduling and sibling work, not CPU time; no idle samples.
pub static PTY_YIELD_WAIT: Histogram = Histogram::new();

// --- echo: input in, output out, same pane --------------------------------

/// Microseconds from input to the PTY writer until the pane's next output.
///
/// Armed only after [`ECHO_QUIET_WINDOW`] of silence. Includes the child's
/// reaction time; samples over [`ECHO_SAMPLE_CEILING`] are dropped.
pub static ECHO_SERVER: Histogram = Histogram::new();

// --- input ----------------------------------------------------------------

/// Input requests queued to a PTY writer.
pub static INPUT_EVENTS: Counter = Counter::new();
/// Microseconds for the writer thread's `write(2)` plus flush of one request.
pub static INPUT_PTY_WRITE: Histogram = Histogram::new();
/// Input events that waited for a saturated pane to drain (ADR-0144).
pub static INPUT_CREDIT_WAITS: Counter = Counter::new();
/// Wall microseconds in an actual bounded credit wait, including timeout.
/// Immediate acquisitions and already-stalled refusals are not sampled.
pub static INPUT_CREDIT_WAIT: Histogram = Histogram::new();
/// Credit waits that reached their deadline (not subsequent fast refusals).
pub static INPUT_CREDIT_TIMEOUTS: Counter = Counter::new();
/// Microseconds from writer-queue send attempt to writer pickup, before the
/// canonical-mode guard. Includes scheduling; terminal replies are sampled too.
pub static INPUT_WRITER_QUEUE_WAIT: Histogram = Histogram::new();
/// Writer-queue refusals, including slots reserved for credited input.
pub static INPUT_WRITER_FULL: Counter = Counter::new();
/// Writer requests refused by the canonical line-discipline guard.
pub static INPUT_CANONICAL_REFUSED: Counter = Counter::new();

// --- actor-derived work ---------------------------------------------------

/// Microseconds per installed detector tick, including viewport projection,
/// process observation and best-effort event emission; no absent-detector samples.
pub static AGENT_DETECT: Histogram = Histogram::new();
/// Microseconds projecting a viewport requested by the detector, including
/// failed projections. On-loan terminals are not sampled.
pub static AGENT_VIEWPORT: Histogram = Histogram::new();
/// Microseconds handling one detector event in the runtime: metadata/authority
/// work, ask broadcast and hook enqueue (not execution of the hook process).
pub static AGENT_PUBLISH: Histogram = Histogram::new();
/// Microseconds querying the live child's kernel cwd, including failed queries;
/// excludes path conversion, comparison and event emission.
pub static PROC_CWD_QUERY: Histogram = Histogram::new();
/// Wall microseconds past an existing actor detector timer's scheduled deadline.
///
/// Includes runtime/OS scheduling and actor priority starvation. The first tick
/// after intentional native-bootstrap gating is skipped; no new wakeups.
pub static RUNTIME_DETECT_TICK_LATE: Histogram = Histogram::new();

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

// --- bootstrap ------------------------------------------------------------

/// Microseconds synthesizing a VT snapshot in the actor.
///
/// Includes scrollback and bounded refusals for raw/state-sync attach and
/// resize/gap resyncs; excludes queue wait, reply handoff and capability adaptation.
pub static BOOTSTRAP_SYNTH: Histogram = Histogram::new();
/// Microseconds initializing one native capture: validation, engine loan and
/// initial scratch allocation. Includes refused starts; excludes backlog wait.
pub static BOOTSTRAP_NATIVE_BEGIN: Histogram = Histogram::new();
/// Microseconds per productive native bootstrap actor step.
///
/// Includes engine export, scratch growth/retention, completion and failure
/// handling/replay. No pending capture means no sample; excludes inter-turn scheduling.
pub static BOOTSTRAP_NATIVE_STEP: Histogram = Histogram::new();

/// The Terminal's rows of the `GET_PERF` report, in render order.
pub static TABLE: &[Metric] = &[
    Metric::histogram("pty.read.size", Unit::Bytes, &PTY_READ_SIZE),
    Metric::counter("pty.read.bytes", Unit::Bytes, &PTY_READ_BYTES),
    Metric::counter("pty.reader.blocked", Unit::Count, &PTY_READER_BLOCKED),
    Metric::histogram("pty.queue_wait", Unit::Micros, &PTY_QUEUE_WAIT),
    Metric::histogram("pty.burst.bytes", Unit::Bytes, &PTY_BURST_BYTES),
    Metric::histogram("pty.burst.chunks", Unit::Count, &PTY_BURST_CHUNKS),
    Metric::histogram("pty.vt_apply", Unit::Micros, &PTY_VT_APPLY),
    Metric::histogram("pty.vt_parse", Unit::Micros, &PTY_VT_PARSE),
    Metric::histogram("pty.post_apply", Unit::Micros, &PTY_POST_APPLY),
    Metric::histogram("pty.yield_wait", Unit::Micros, &PTY_YIELD_WAIT),
    Metric::histogram("echo.server", Unit::Micros, &ECHO_SERVER),
    Metric::counter("input.events", Unit::Count, &INPUT_EVENTS),
    Metric::histogram("input.pty_write", Unit::Micros, &INPUT_PTY_WRITE),
    Metric::counter("input.credit_waits", Unit::Count, &INPUT_CREDIT_WAITS),
    Metric::histogram("input.credit_wait", Unit::Micros, &INPUT_CREDIT_WAIT),
    Metric::counter("input.credit_timeouts", Unit::Count, &INPUT_CREDIT_TIMEOUTS),
    Metric::histogram(
        "input.writer.queue_wait",
        Unit::Micros,
        &INPUT_WRITER_QUEUE_WAIT,
    ),
    Metric::counter("input.writer.full", Unit::Count, &INPUT_WRITER_FULL),
    Metric::counter(
        "input.canonical_refused",
        Unit::Count,
        &INPUT_CANONICAL_REFUSED,
    ),
    Metric::histogram("agent.detect", Unit::Micros, &AGENT_DETECT),
    Metric::histogram("agent.viewport", Unit::Micros, &AGENT_VIEWPORT),
    Metric::histogram("agent.publish", Unit::Micros, &AGENT_PUBLISH),
    Metric::histogram("proc.cwd_query", Unit::Micros, &PROC_CWD_QUERY),
    Metric::histogram(
        "runtime.detect_tick_late",
        Unit::Micros,
        &RUNTIME_DETECT_TICK_LATE,
    ),
    Metric::histogram("tick.emit", Unit::Micros, &TICK_EMIT),
    Metric::histogram("tick.synth", Unit::Micros, &TICK_SYNTH),
    Metric::histogram("tick.out_bytes", Unit::Bytes, &TICK_OUT_BYTES),
    Metric::counter("consumer.mailbox_full", Unit::Count, &CONSUMER_MAILBOX_FULL),
    Metric::counter("consumer.reaped", Unit::Count, &CONSUMER_REAPED),
    Metric::histogram("consumer.ack_rtt", Unit::Micros, &CONSUMER_ACK_RTT),
    Metric::histogram("bootstrap.synth", Unit::Micros, &BOOTSTRAP_SYNTH),
    Metric::histogram(
        "bootstrap.native_begin",
        Unit::Micros,
        &BOOTSTRAP_NATIVE_BEGIN,
    ),
    Metric::histogram(
        "bootstrap.native_step",
        Unit::Micros,
        &BOOTSTRAP_NATIVE_STEP,
    ),
];

/// Server-side echo samples longer than this are a program that did not
/// echo, not a slow server, and are dropped rather than skewing the tail.
pub const ECHO_SAMPLE_CEILING: std::time::Duration = std::time::Duration::from_secs(2);

/// Quiet time before input arms `echo.server`.
pub const ECHO_QUIET_WINDOW: std::time::Duration = std::time::Duration::from_millis(100);

/// Rate limit shared by the degradation warnings this module owns.
pub const WARN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Warn throttle for a consumer whose mailbox is full.
pub static MAILBOX_FULL_WARN: phux_perf::Throttle = phux_perf::Throttle::new(WARN_INTERVAL);

/// Promote a helper thread on the keystroke path (refusal is only logged).
pub fn promote_helper_thread(name: &str) {
    if !phux_perf::promote_current_thread() {
        tracing::debug!(thread = name, "interactive scheduling not granted");
    }
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
}
