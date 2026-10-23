//! Desktop host telemetry: what painting runtime frames costs the UI thread,
//! and how long a key takes to reach the screen.
//!
//! The kernel table counts output frames and echo round trips, and the
//! runtime table counts publications; neither sees the window. These rows sit
//! one layer out, on the GPUI thread. A terminal entity renders only when it is notified or
//! its window is refreshed
//! (`desktop.render`): it acquires and validates its frame (`desktop.acquire`,
//! one owner round trip), prepares it (`desktop.prepare`: reusing the last
//! scene when nothing it depends on changed, `desktop.prepare_reused`, or
//! keeps unchanged rows and reshapes the rest, `desktop.prepare_patched`,
//! else a cell walk with `desktop.shaped` line shapes), paints it (`desktop.paint`)
//! and hands the painted geometry to input (`desktop.present`).
//! `desktop.key_to_paint` runs from a key reaching a focused terminal to the
//! first paint of that terminal's next output. Everything is a `static` from
//! [`phux_perf`], always on, one relaxed atomic add per sample.
//! The desktop's `diag::report` appends them to the kernel and runtime rows.

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use phux_perf::{Counter, Histogram, Metric, Unit};

/// Terminal element renders: one per element per window draw.
pub static RENDERS: Counter = Counter::new();
/// Microseconds to acquire and validate one element's frame (UI thread,
/// including its owner round trip).
pub static ACQUIRE: Histogram = Histogram::new();
/// Acquires that found no paintable frame (stale, detached or not ready):
/// each paints an empty surface for that draw.
pub static ACQUIRE_REJECTED: Counter = Counter::new();
/// Microseconds to prepare one terminal element's frame for paint.
pub static PREPARE: Histogram = Histogram::new();
/// Prepares that reused the element's previous scene.
pub static PREPARE_REUSED: Counter = Counter::new();
/// Prepares that kept the shaped rows of the previous scene and reshaped
/// only the rows whose cells or solid cursor changed.
pub static PREPARE_PATCHED: Counter = Counter::new();
/// Line layouts prepared (runs and lone cells), including cached ASCII assembly.
pub static SHAPED: Counter = Counter::new();
/// Calls into the platform text shaper, including ASCII table warmup.
pub static PLATFORM_SHAPED: Counter = Counter::new();
/// Printable-ASCII layouts assembled from validated bundled-font tables.
pub static ASCII_LINES: Counter = Counter::new();
/// Microseconds to submit one terminal element's quads and glyphs.
pub static PAINT: Histogram = Histogram::new();
/// Microseconds to hand a painted frame's geometry to its input handler.
pub static PRESENT: Histogram = Histogram::new();
/// Microseconds from a key reaching a focused terminal to the first paint of
/// that terminal's next output (the echo, when a program answers).
pub static KEY_TO_PAINT: Histogram = Histogram::new();

/// Samples longer than this are a program that did not answer, not a slow
/// path (mirrors the kernel's echo ceiling).
const KEY_SAMPLE_CEILING: Duration = Duration::from_secs(2);

/// The desktop host's metric table, in render order.
pub static TABLE: &[Metric] = &[
    Metric::histogram("desktop.key_to_paint", Unit::Micros, &KEY_TO_PAINT),
    Metric::counter("desktop.render", Unit::Count, &RENDERS),
    Metric::histogram("desktop.acquire", Unit::Micros, &ACQUIRE),
    Metric::counter("desktop.acquire_rejected", Unit::Count, &ACQUIRE_REJECTED),
    Metric::histogram("desktop.prepare", Unit::Micros, &PREPARE),
    Metric::counter("desktop.prepare_reused", Unit::Count, &PREPARE_REUSED),
    Metric::counter("desktop.prepare_patched", Unit::Count, &PREPARE_PATCHED),
    Metric::counter("desktop.shaped", Unit::Count, &SHAPED),
    Metric::counter("desktop.platform_shaped", Unit::Count, &PLATFORM_SHAPED),
    Metric::counter("desktop.ascii_lines", Unit::Count, &ASCII_LINES),
    Metric::histogram("desktop.paint", Unit::Micros, &PAINT),
    Metric::histogram("desktop.present", Unit::Micros, &PRESENT),
];

static STARTED: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Time since the host started; the desktop's `diag::report` counts from here.
#[must_use]
pub fn uptime() -> Duration {
    STARTED.elapsed()
}

/// Mark the host's start, so uptime counts from initialization.
pub fn mark_started() {
    LazyLock::force(&STARTED);
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
}
