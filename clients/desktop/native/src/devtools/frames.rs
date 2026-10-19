//! The recent-frame ring: every desk render, its build time and the gap
//! since the frame before, for FPS, jank and the HUD sparkline.
//!
//! Always on. One uncontended lock and a push per frame; GPUI renders on one
//! thread, so the lock only meets a reader (the HUD, the dev socket).

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;

/// Frames kept: ten seconds at 120 Hz.
pub const CAPACITY: usize = 1200;

/// A frame whose build takes longer than this misses a 120 Hz deadline
/// before layout and paint even start.
pub const BUILD_BUDGET: Duration = Duration::from_micros(8_333);

/// A gap between frames longer than this, inside an animation, is a dropped
/// 60 Hz frame. Gaps longer than [`IDLE_GAP`] are idle time, not jank.
pub const JANK_GAP: Duration = Duration::from_micros(16_667 * 3 / 2);

/// A gap this long means nothing was asking for frames.
pub const IDLE_GAP: Duration = Duration::from_millis(250);

static RING: Mutex<Ring> = Mutex::new(Ring {
    frames: VecDeque::new(),
    last_start: None,
});

struct Ring {
    frames: VecDeque<Frame>,
    last_start: Option<Instant>,
}

/// One rendered frame.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    /// When the render began.
    pub at: Instant,
    /// Time spent building the desk's element tree.
    pub build: Duration,
    /// Time since the previous render began; `None` for the first.
    pub gap: Option<Duration>,
}

/// Times one desk render into the ring and [`crate::diag::FRAME`] on drop.
#[derive(Debug)]
pub struct FrameTimer {
    started: Instant,
}

impl FrameTimer {
    /// Start timing a render.
    #[must_use]
    pub fn start() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl Drop for FrameTimer {
    fn drop(&mut self) {
        let build = self.started.elapsed();
        crate::diag::FRAME.record_duration(build);
        record(self.started, build);
    }
}

fn record(at: Instant, build: Duration) {
    let mut ring = RING.lock().unwrap_or_else(PoisonError::into_inner);
    let gap = ring
        .last_start
        .map(|last| at.saturating_duration_since(last));
    ring.last_start = Some(at);
    if ring.frames.len() == CAPACITY {
        ring.frames.pop_front();
    }
    ring.frames.push_back(Frame { at, build, gap });
}

/// The frames that began within `window` of now, oldest first.
#[must_use]
pub fn recent(window: Duration) -> Vec<Frame> {
    let now = Instant::now();
    let ring = RING.lock().unwrap_or_else(PoisonError::into_inner);
    ring.frames
        .iter()
        .filter(|frame| now.saturating_duration_since(frame.at) <= window)
        .copied()
        .collect()
}

/// The last `n` frames, oldest first.
#[must_use]
pub fn last(n: usize) -> Vec<Frame> {
    let ring = RING.lock().unwrap_or_else(PoisonError::into_inner);
    let skip = ring.frames.len().saturating_sub(n);
    ring.frames.iter().skip(skip).copied().collect()
}

/// What the last `window` of frames looked like.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Summary {
    /// Frames per second over the window.
    pub fps: f64,
    /// Frames in the window.
    pub frames: usize,
    /// Median build time, microseconds.
    pub build_p50_us: u64,
    /// 99th-percentile build time, microseconds.
    pub build_p99_us: u64,
    /// Longest build, microseconds.
    pub build_max_us: u64,
    /// Builds over [`BUILD_BUDGET`].
    pub over_budget: usize,
    /// Gaps between [`JANK_GAP`] and [`IDLE_GAP`]: frames dropped while
    /// something was animating or streaming.
    pub janks: usize,
    /// Longest non-idle gap, microseconds.
    pub worst_gap_us: u64,
}

/// Summarize `frames`, which span `window`.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "a frame count per second is far below f64's exact range"
)]
pub fn summarize(frames: &[Frame], window: Duration) -> Summary {
    if frames.is_empty() {
        return Summary::default();
    }
    let mut builds: Vec<u64> = frames.iter().map(|f| micros(f.build)).collect();
    builds.sort_unstable();
    let busy_gaps = frames
        .iter()
        .filter_map(|f| f.gap)
        .filter(|gap| *gap < IDLE_GAP);
    let (janks, worst_gap) = busy_gaps.fold((0, Duration::ZERO), |(janks, worst), gap| {
        (janks + usize::from(gap > JANK_GAP), worst.max(gap))
    });
    Summary {
        fps: frames.len() as f64 / window.as_secs_f64().max(f64::EPSILON),
        frames: frames.len(),
        build_p50_us: percentile(&builds, 50),
        build_p99_us: percentile(&builds, 99),
        build_max_us: builds.last().copied().unwrap_or_default(),
        over_budget: frames.iter().filter(|f| f.build > BUILD_BUDGET).count(),
        janks,
        worst_gap_us: micros(worst_gap),
    }
}

/// [`summarize`] over the last second.
#[must_use]
pub fn last_second() -> Summary {
    let window = Duration::from_secs(1);
    summarize(&recent(window), window)
}

/// Nearest-rank percentile of sorted samples.
fn percentile(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p * sorted.len()).div_ceil(100).max(1);
    sorted[rank.min(sorted.len()) - 1]
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(at: Instant, build_ms: u64, gap_ms: Option<u64>) -> Frame {
        Frame {
            at,
            build: Duration::from_millis(build_ms),
            gap: gap_ms.map(Duration::from_millis),
        }
    }

    #[test]
    fn summary_separates_jank_from_idle() {
        let now = Instant::now();
        let frames = [
            frame(now, 2, None),
            frame(now, 3, Some(16)),
            frame(now, 12, Some(40)),
            frame(now, 1, Some(2_000)),
        ];
        let s = summarize(&frames, Duration::from_secs(1));
        assert_eq!(s.frames, 4);
        assert!((s.fps - 4.0).abs() < f64::EPSILON);
        assert_eq!(s.over_budget, 1, "only the 12 ms build misses 8.3 ms");
        assert_eq!(s.janks, 1, "40 ms is a dropped frame; 2 s is idle");
        assert_eq!(s.worst_gap_us, 40_000);
        assert_eq!(s.build_max_us, 12_000);
        assert_eq!(s.build_p50_us, 2_000);
    }

    #[test]
    fn empty_window_is_zero() {
        assert_eq!(summarize(&[], Duration::from_secs(1)), Summary::default());
    }

    #[test]
    fn timer_feeds_the_ring() {
        drop(FrameTimer::start());
        drop(FrameTimer::start());
        let frames = last(2);
        assert_eq!(frames.len(), 2);
        assert!(frames[1].gap.is_some());
    }
}
