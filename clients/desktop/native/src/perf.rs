//! Desktop host telemetry: what painting runtime frames costs the UI thread,
//! and how long a key takes to reach the screen.
//!
//! The kernel table counts output frames and echo round trips, and the
//! runtime table counts publications; neither sees the window. These rows sit
//! one layer out, on the GPUI thread: one prepare (cell walk, colours, glyph
//! shaping) and one paint (quad and glyph submission) per terminal element per
//! window draw, and `desktop.key_to_paint`, from a key reaching a focused
//! terminal to the first paint of that terminal's next output. Everything is a
//! `static` from [`phux_perf`], always on, one relaxed atomic add per sample.
//! `desktopPerfJson` appends them to the kernel and runtime rows.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use napi_derive::napi;
use phux_perf::{Counter, Histogram, Metric, Unit};

/// Microseconds to prepare one terminal element's frame for paint.
pub static PREPARE: Histogram = Histogram::new();
/// Microseconds to submit one terminal element's quads and glyphs.
pub static PAINT: Histogram = Histogram::new();
/// Prepares that reused the previous draw's cell walk and shaped glyphs.
pub static PREPARE_REUSED: Counter = Counter::new();
/// Microseconds from a key reaching a focused terminal to the first paint of
/// that terminal's next output (the echo, when a program answers).
pub static KEY_TO_PAINT: Histogram = Histogram::new();

/// Samples longer than this are a program that did not answer, not a slow
/// path (mirrors the kernel's echo ceiling).
pub const KEY_SAMPLE_CEILING: Duration = Duration::from_secs(2);

/// The desktop host's metric table, in render order.
pub static TABLE: &[Metric] = &[
    Metric::histogram("desktop.key_to_paint", Unit::Micros, &KEY_TO_PAINT),
    Metric::histogram("desktop.prepare", Unit::Micros, &PREPARE),
    Metric::histogram("desktop.paint", Unit::Micros, &PAINT),
    Metric::counter("desktop.prepare_reused", Unit::Count, &PREPARE_REUSED),
];

fn started() -> Instant {
    static STARTED: OnceLock<Instant> = OnceLock::new();
    *STARTED.get_or_init(Instant::now)
}

/// Kernel, runtime and desktop rows as one `PerfReport` JSON, counted since
/// the host loaded. Process-wide: every window and connection shares them.
#[napi]
#[must_use]
pub fn desktop_perf_json() -> String {
    let mut report = phux_client_core::perf::report(started().elapsed());
    report.metrics.extend(phux_client_runtime::perf::snapshot());
    report.metrics.extend(TABLE.iter().map(Metric::snapshot));
    report.to_json()
}

/// Mark the host's start, so uptime counts from initialization.
pub(crate) fn mark_started() {
    let _ = started();
}

/// One terminal's key-to-paint arming: a key arms it with the output
/// sequence then on screen, and the first paint past that sequence samples.
#[derive(Debug, Default)]
pub struct KeyProbe {
    armed: Option<(Instant, u64)>,
}

impl KeyProbe {
    /// A key left for the terminal while `painted_seq` was on screen. A burst
    /// keeps its first mark, so typing measures from the first key.
    pub fn arm(&mut self, painted_seq: u64) {
        self.armed.get_or_insert((Instant::now(), painted_seq));
    }

    /// A frame at `seq` was painted: close an open sample it answers.
    pub fn painted(&mut self, seq: u64) {
        let Some((at, before)) = self.armed else {
            return;
        };
        if seq <= before {
            return;
        }
        self.armed = None;
        let elapsed = at.elapsed();
        if elapsed < KEY_SAMPLE_CEILING {
            KEY_TO_PAINT.record_duration(elapsed);
        }
    }

    /// The presentation changed identity; an open sample would span it.
    pub fn forget(&mut self) {
        self.armed = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_samples_only_a_later_sequence_once() {
        let before = KEY_TO_PAINT.count();
        let mut probe = KeyProbe::default();
        probe.painted(9);
        probe.arm(4);
        probe.arm(5);
        probe.painted(4);
        assert_eq!(KEY_TO_PAINT.count(), before, "no newer output yet");
        probe.painted(5);
        assert_eq!(KEY_TO_PAINT.count(), before + 1);
        probe.painted(6);
        assert_eq!(KEY_TO_PAINT.count(), before + 1, "one sample per arm");
        probe.arm(6);
        probe.forget();
        probe.painted(7);
        assert_eq!(KEY_TO_PAINT.count(), before + 1);
    }

    #[test]
    fn report_carries_every_layer() {
        let json = desktop_perf_json();
        for name in ["kernel.frames", "runtime.publish", "desktop.key_to_paint"] {
            assert!(json.contains(name), "{name} missing from {json}");
        }
    }
}
