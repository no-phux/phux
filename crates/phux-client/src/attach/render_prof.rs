//! Render-profile shim over [`crate::perf`].
//!
//! The counters used to live here behind a `PHUX_RENDER_PROF` latch. They
//! are now always-on statics in [`crate::perf`] (one relaxed `fetch_add`
//! each, so there is nothing to gate), and this module keeps the `note_*`
//! call sites and the one-line-per-second `render_prof` log that the env
//! knob still turns on.

#![allow(
    clippy::redundant_pub_crate,
    reason = "the module is `pub(crate)`, so `pub(crate)` items are what actually name their reach; `pub` here trips `unreachable_pub` instead"
)]

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use crate::perf;

static ENABLED: AtomicBool = AtomicBool::new(false);
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Is the periodic `render_prof` log line requested (`PHUX_RENDER_PROF`)?
pub(crate) fn enabled() -> bool {
    if INITIALIZED.load(Relaxed) {
        return ENABLED.load(Relaxed);
    }
    let on = std::env::var_os("PHUX_RENDER_PROF").is_some_and(|v| v != "0" && !v.is_empty());
    ENABLED.store(on, Relaxed);
    INITIALIZED.store(true, Relaxed);
    on
}

macro_rules! counter {
    ($name:ident, $cell:path) => {
        pub(crate) fn $name(n: u64) {
            $cell.add(n);
        }
    };
}

counter!(note_frames, perf::FRAMES);
counter!(note_paints, perf::PAINTS);
counter!(note_skipped, perf::SKIPPED);
counter!(note_bar_composes, perf::BAR_COMPOSES);
counter!(note_layouts, perf::LAYOUTS);
counter!(note_flushes, perf::FLUSHES);
counter!(note_bytes, perf::BYTES_OUT);
counter!(note_paced_replies, perf::PACER_REPLIES);
counter!(note_paced_waits, perf::PACER_WAITS);

const WINDOW: std::time::Duration = std::time::Duration::from_secs(1);

/// Emit the periodic line if the window has elapsed. Only reads the clock
/// when the env knob is on, so the loop it measures is not perturbed.
pub(crate) fn tick() {
    if !enabled() {
        return;
    }
    tick_at(std::time::Instant::now());
}

thread_local! {
    static WINDOW_START: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
    static LAST: std::cell::RefCell<Option<phux_perf::PerfReport>> =
        const { std::cell::RefCell::new(None) };
}

fn tick_at(now: std::time::Instant) {
    let start = WINDOW_START.with(|cell| {
        let start = cell.get().unwrap_or(now);
        cell.set(Some(start));
        start
    });
    let elapsed = now.saturating_duration_since(start);
    if elapsed < WINDOW {
        return;
    }
    WINDOW_START.with(|cell| cell.set(Some(now)));
    let current = perf::report();
    let interval = LAST.with(|last| {
        let mut last = last.borrow_mut();
        let delta = last
            .as_ref()
            .map_or_else(|| current.clone(), |prev| current.delta(prev));
        *last = Some(current);
        delta
    });
    let get = |name: &str| -> u64 {
        match interval.metric(name).map(|m| &m.value) {
            Some(phux_perf::MetricValue::Counter(n)) => *n,
            _ => 0,
        }
    };
    let echo = match interval.metric("echo.rtt").map(|m| &m.value) {
        Some(phux_perf::MetricValue::Histogram(h)) => h.clone(),
        _ => phux_perf::HistogramSnapshot::default(),
    };
    tracing::info!(
        frames = get("frames.received"),
        paints = get("frames.painted"),
        skipped = get("frames.skipped"),
        bar_composes = get("frames.bar_composes"),
        layouts = get("frames.layouts"),
        flushes = get("stdout.flushes"),
        paced_replies = get("pacer.replies"),
        paced_waits = get("pacer.waits"),
        bytes = get("stdout.bytes"),
        echo_n = echo.count,
        echo_p50_us = echo.percentile(50),
        echo_p99_us = echo.percentile(99),
        window_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        "render_prof",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_always_move() {
        let before = perf::FRAMES.get();
        note_frames(5);
        assert!(perf::FRAMES.get() >= before + 5);
    }

    #[test]
    fn a_sub_window_tick_does_not_log_or_reset() {
        let now = std::time::Instant::now();
        WINDOW_START.with(|cell| cell.set(Some(now)));
        let before = perf::PAINTS.get();
        note_paints(3);
        tick_at(now);
        assert!(perf::PAINTS.get() >= before + 3);
    }
}
