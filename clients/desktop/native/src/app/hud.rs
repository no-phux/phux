//! The performance HUD: a small overlay in the window's top-right corner
//! with the readings worth glancing at while working, so a stall is noticed
//! when it happens, not reconstructed afterwards.
//!
//! Shown by [`crate::devtools::hud_visible`]; the pacer in
//! [`crate::devtools`] repaints twice a second while it is up.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, SharedString, div, hsla, px};
use phux_perf::alloc::AllocStats;

use crate::devtools::frames::{self, Frame, Summary};
use crate::devtools::server::ServerSummary;
use crate::telemetry::Sampler;

/// Frames drawn in the sparkline: two seconds at 60 Hz.
const SPARK_FRAMES: usize = 120;
/// Sparkline height in pixels; a bar this tall is a 16.7 ms build.
const SPARK_HEIGHT: f32 = 24.;
const SPARK_FULL_SCALE: Duration = Duration::from_micros(16_667);

/// Process readings, refreshed at most every half second.
struct Readings {
    sampler: Sampler,
    at: Option<Instant>,
    cpu: Option<f64>,
    rss: Option<u64>,
    alloc: Option<AllocStats>,
    allocs_per_sec: Option<f64>,
}

static READINGS: Mutex<Option<Readings>> = Mutex::new(None);

/// What the HUD says, as plain lines; the element only lays them out.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Lines {
    pub frames: String,
    pub process: String,
    pub heap: Option<String>,
    pub server: Option<String>,
}

#[allow(
    clippy::cast_precision_loss,
    reason = "allocation counts per second, for display"
)]
fn readings() -> (Option<f64>, Option<u64>, Option<AllocStats>, Option<f64>) {
    let mut slot = READINGS.lock().unwrap_or_else(PoisonError::into_inner);
    let readings = slot.get_or_insert_with(|| Readings {
        sampler: Sampler::new(),
        at: None,
        cpu: None,
        rss: None,
        alloc: None,
        allocs_per_sec: None,
    });
    let now = Instant::now();
    let stale = readings
        .at
        .is_none_or(|at| now.saturating_duration_since(at) >= Duration::from_millis(450));
    if stale {
        let sample = readings.sampler.sample();
        let alloc = sample.report.alloc;
        readings.allocs_per_sec = match (alloc, readings.alloc, readings.at) {
            (Some(now_alloc), Some(before), Some(at)) => {
                let secs = now.saturating_duration_since(at).as_secs_f64();
                (secs > 0.0).then(|| now_alloc.delta(&before).allocs as f64 / secs)
            }
            _ => None,
        };
        readings.cpu = sample.cpu_percent.or(readings.cpu);
        readings.rss = sample.resident_bytes;
        readings.alloc = alloc;
        readings.at = Some(now);
    }
    (
        readings.cpu,
        readings.rss,
        readings.alloc,
        readings.allocs_per_sec,
    )
}

/// The HUD's text for the current readings.
pub(crate) fn lines() -> Lines {
    let summary = frames::last_second();
    let (cpu, rss, alloc, allocs_per_sec) = readings();
    let server = crate::devtools::get().poller.summary();
    compose(&summary, cpu, rss, alloc, allocs_per_sec, &server)
}

fn compose(
    summary: &Summary,
    cpu: Option<f64>,
    rss: Option<u64>,
    alloc: Option<AllocStats>,
    allocs_per_sec: Option<f64>,
    server: &ServerSummary,
) -> Lines {
    let frames = format!(
        "{:>3.0} fps  build p50 {} p99 {} max {}  jank {}",
        summary.fps,
        ms(summary.build_p50_us),
        ms(summary.build_p99_us),
        ms(summary.build_max_us),
        summary.janks,
    );
    let process = format!(
        "cpu {}  rss {}",
        cpu.map_or_else(dash, |cpu| format!("{cpu:.0}%")),
        rss.map_or_else(dash, bytes),
    );
    let heap = alloc.map(|alloc| {
        format!(
            "heap {} peak {}  {} allocs/s",
            signed_bytes(alloc.live_bytes),
            signed_bytes(alloc.peak_bytes),
            allocs_per_sec.map_or_else(dash, count),
        )
    });
    let server = match (&server.error, server.sampled) {
        (Some(error), _) => Some(format!("server: {}", first_line(error))),
        (None, true) => Some(format!(
            "server cpu {}  rss {}",
            server
                .cpu_percent
                .map_or_else(dash, |cpu| format!("{cpu:.0}%")),
            server.rss_bytes.map_or_else(dash, bytes),
        )),
        (None, false) => None,
    };
    Lines {
        frames,
        process,
        heap,
        server,
    }
}

/// The HUD element, or nothing while hidden.
pub(crate) fn element(mono: SharedString) -> Option<AnyElement> {
    if !crate::devtools::hud_visible() {
        return None;
    }
    let lines = lines();
    let ink = hsla(0., 0., 0.92, 1.);
    let dim = hsla(0., 0., 0.70, 1.);
    let text = |line: String, color| div().text_color(color).child(line);
    Some(
        div()
            .id("perf-hud")
            .absolute()
            .top(px(44.))
            .right(px(12.))
            .p(px(8.))
            .rounded(px(6.))
            .bg(hsla(0., 0., 0.06, 0.82))
            .font_family(mono)
            .text_xs()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(text(lines.frames, ink))
            .child(text(lines.process, ink))
            .children(lines.heap.map(|line| text(line, ink)))
            .children(lines.server.map(|line| text(line, dim)))
            .child(sparkline(&frames::last(SPARK_FRAMES)))
            .into_any_element(),
    )
}

/// One bar per frame, height by build time; amber past the 120 Hz build
/// budget, red past a 60 Hz frame.
fn sparkline(frames: &[Frame]) -> impl IntoElement {
    let bars = frames.iter().map(|frame| {
        let fraction = frame.build.as_secs_f32() / SPARK_FULL_SCALE.as_secs_f32();
        let color = if frame.build > SPARK_FULL_SCALE {
            hsla(0.0, 0.85, 0.58, 1.)
        } else if frame.build > frames::BUILD_BUDGET {
            hsla(0.11, 0.9, 0.55, 1.)
        } else {
            hsla(0.36, 0.6, 0.55, 1.)
        };
        div()
            .w(px(2.))
            .h(px((fraction.clamp(0.04, 1.) * SPARK_HEIGHT).max(1.)))
            .bg(color)
    });
    div()
        .mt(px(4.))
        .h(px(SPARK_HEIGHT))
        .flex()
        .items_end()
        .gap(px(1.))
        .children(bars)
}

fn dash() -> String {
    "—".to_owned()
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

#[allow(
    clippy::cast_precision_loss,
    reason = "millisecond display of microsecond samples"
)]
fn ms(us: u64) -> String {
    format!("{:.1}ms", us as f64 / 1000.0)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "approximate display of large counts"
)]
fn count(n: f64) -> String {
    if n >= 1_000_000.0 {
        format!("{:.1}M", n / 1_000_000.0)
    } else if n >= 1_000.0 {
        format!("{:.1}k", n / 1_000.0)
    } else {
        format!("{n:.0}")
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "approximate display of byte counts"
)]
fn bytes(b: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    let mib = b as f64 / MIB;
    if mib >= 1024.0 {
        format!("{:.2}GiB", mib / 1024.0)
    } else {
        format!("{mib:.0}MiB")
    }
}

fn signed_bytes(b: i64) -> String {
    let magnitude = bytes(b.unsigned_abs());
    if b < 0 {
        format!("-{magnitude}")
    } else {
        magnitude
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_show_what_is_known_and_dash_the_rest() {
        let summary = Summary {
            fps: 59.6,
            frames: 60,
            build_p50_us: 1_200,
            build_p99_us: 4_100,
            build_max_us: 7_000,
            over_budget: 0,
            janks: 2,
            worst_gap_us: 40_000,
        };
        let alloc = AllocStats {
            live_bytes: 50 * 1024 * 1024,
            peak_bytes: 80 * 1024 * 1024,
            ..AllocStats::default()
        };
        let lines = compose(
            &summary,
            None,
            Some(200 * 1024 * 1024),
            Some(alloc),
            Some(12_345.0),
            &ServerSummary::default(),
        );
        assert_eq!(
            lines.frames,
            " 60 fps  build p50 1.2ms p99 4.1ms max 7.0ms  jank 2"
        );
        assert_eq!(lines.process, "cpu —  rss 200MiB");
        assert_eq!(
            lines.heap.as_deref(),
            Some("heap 50MiB peak 80MiB  12.3k allocs/s")
        );
        assert_eq!(lines.server, None);
        let failed = compose(
            &summary,
            Some(3.0),
            None,
            None,
            None,
            &ServerSummary {
                error: Some("no server\nmore".to_owned()),
                ..ServerSummary::default()
            },
        );
        assert_eq!(failed.heap, None);
        assert_eq!(failed.server.as_deref(), Some("server: no server"));
        let server = compose(
            &summary,
            None,
            None,
            None,
            None,
            &ServerSummary {
                cpu_percent: Some(4.4),
                rss_bytes: Some(40 * 1024 * 1024),
                error: None,
                sampled: true,
            },
        );
        assert_eq!(server.server.as_deref(), Some("server cpu 4%  rss 40MiB"));
    }
}
