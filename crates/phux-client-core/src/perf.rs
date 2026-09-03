//! Kernel-side performance telemetry, shared by every frontend.
//!
//! The ratatui client, the native cockpit (through `phux-client-ffi`), and the
//! web client all drive the same [`crate::session::SessionKernel`], so the
//! kernel is where a frontend-neutral view of "how fast is this client"
//! lives: frames applied and their bytes, the time libghostty took to apply
//! each one, and the echo round trip from a key or paste leaving through
//! [`crate::session::KernelSend::Input`] to the first output frame for that
//! terminal coming back. Everything is a `static` from [`phux_perf`], always
//! on, one relaxed atomic add per sample.
//!
//! On `wasm32` there is no monotonic clock, so the two latency histograms
//! stay empty and only the counters move; the report still serialises.

use phux_perf::{Counter, Histogram, Metric, PerfReport, Unit};
use phux_protocol::ids::TerminalId;

/// `TERMINAL_OUTPUT` frames applied to a replica.
pub static OUTPUT_FRAMES: Counter = Counter::new();
/// Payload bytes those frames carried.
pub static OUTPUT_BYTES: Counter = Counter::new();
/// Microseconds the engine took to apply one output frame (native only).
pub static KERNEL_APPLY: Histogram = Histogram::new();
/// Microseconds from a key or paste being handed to the frontend for sending
/// until the first output frame for that terminal arrived (native only).
pub static ECHO_RTT: Histogram = Histogram::new();
/// Key and paste events routed through the kernel.
pub static INPUT_SENT: Counter = Counter::new();

/// The kernel's metric table, in render order.
pub static TABLE: &[Metric] = &[
    Metric::histogram("kernel.echo.rtt", Unit::Micros, &ECHO_RTT),
    Metric::histogram("kernel.apply", Unit::Micros, &KERNEL_APPLY),
    Metric::counter("kernel.frames", Unit::Count, &OUTPUT_FRAMES),
    Metric::counter("kernel.bytes", Unit::Bytes, &OUTPUT_BYTES),
    Metric::counter("kernel.input_sent", Unit::Count, &INPUT_SENT),
];

/// Echo samples longer than this are a program that did not answer, not a
/// slow path, and are dropped rather than skewing the tail.
pub const ECHO_SAMPLE_CEILING: core::time::Duration = core::time::Duration::from_secs(2);

/// Snapshot the kernel table. `uptime` is the frontend's notion of how long
/// this kernel has been alive; the FFI passes the time since the client was
/// created.
#[must_use]
pub fn report(uptime: core::time::Duration) -> PerfReport {
    phux_perf::snapshot("kernel", TABLE, uptime)
}

/// Per-terminal echo arming: a key or paste arms the terminal, the next
/// output frame for it takes the sample.
///
/// One kernel owns one probe. Interior mutability because the kernel arms
/// from its `&self` action path; the kernel is single-threaded by
/// construction, so a `RefCell` is the honest cell.
#[derive(Debug, Default)]
pub struct EchoProbe {
    #[cfg(not(target_arch = "wasm32"))]
    armed: core::cell::RefCell<std::collections::HashMap<TerminalId, std::time::Instant>>,
}

impl EchoProbe {
    /// Record that input which a program is expected to answer just left
    /// for `terminal_id`. A re-arm before the reply keeps the earlier mark,
    /// so a burst of typing measures from its first key.
    pub fn arm(&self, terminal_id: &TerminalId) {
        INPUT_SENT.incr();
        #[cfg(not(target_arch = "wasm32"))]
        self.armed
            .borrow_mut()
            .entry(terminal_id.clone())
            .or_insert_with(std::time::Instant::now);
        #[cfg(target_arch = "wasm32")]
        let _ = terminal_id;
    }

    /// Output arrived for `terminal_id`: close any open sample.
    pub fn observe(&self, terminal_id: &TerminalId) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(at) = self.armed.borrow_mut().remove(terminal_id) {
            let elapsed = at.elapsed();
            if elapsed < ECHO_SAMPLE_CEILING {
                ECHO_RTT.record_duration(elapsed);
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = terminal_id;
    }

    /// The terminal is gone; forget any open sample.
    pub fn forget(&self, terminal_id: &TerminalId) {
        #[cfg(not(target_arch = "wasm32"))]
        self.armed.borrow_mut().remove(terminal_id);
        #[cfg(target_arch = "wasm32")]
        let _ = terminal_id;
    }
}

/// A drop guard timing one engine apply into [`KERNEL_APPLY`].
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn apply_timer() -> phux_perf::Timer {
    KERNEL_APPLY.timer()
}

/// No monotonic clock on wasm: the apply is counted, not timed.
#[cfg(target_arch = "wasm32")]
pub const fn apply_timer() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_names_are_unique_and_prefixed() {
        let mut names: Vec<&str> = TABLE.iter().map(|m| m.name).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n);
        assert!(TABLE.iter().all(|m| m.name.starts_with("kernel.")));
    }

    #[test]
    fn report_is_tagged_kernel() {
        let r = report(core::time::Duration::from_secs(1));
        assert_eq!(r.role, "kernel");
        assert_eq!(r.metrics.len(), TABLE.len());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn echo_probe_samples_once_per_arm_and_forgets_on_close() {
        let id = TerminalId::Local { id: 7 };
        let other = TerminalId::Local { id: 8 };
        let before = ECHO_RTT.count();
        let probe = EchoProbe::default();
        probe.observe(&id);
        assert_eq!(
            ECHO_RTT.count(),
            before,
            "output without input is not an echo"
        );
        probe.arm(&id);
        probe.arm(&id);
        probe.observe(&other);
        assert_eq!(
            ECHO_RTT.count(),
            before,
            "another terminal's output does not close it"
        );
        probe.observe(&id);
        assert_eq!(ECHO_RTT.count(), before + 1);
        probe.observe(&id);
        assert_eq!(ECHO_RTT.count(), before + 1, "one arm, one sample");
        probe.arm(&id);
        probe.forget(&id);
        probe.observe(&id);
        assert_eq!(
            ECHO_RTT.count(),
            before + 1,
            "a closed terminal drops its sample"
        );
    }
}
