//! Playback scheduling for `phux play` (ADR-0064): when is each event due?
//!
//! [`due_at`] returns an offset from playback **start**, never from the
//! previous event: a driver that sleeps until `start + due_at(t)` absorbs late
//! wakeups instead of accumulating them. `--speed` divides every deadline and
//! never resamples, drops, or merges events.

use std::time::Duration;

use crate::cast::CastEvent;

/// A validated playback rate: finite and within [`Speed::MIN`]..=[`Speed::MAX`].
///
/// Every deadline divides by it, so zero, negative, and NaN rates (which
/// would hang a player or flush a recording in one tick) are rejected once
/// at the parse boundary.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Speed(f64);

impl Speed {
    /// Real time.
    pub const NORMAL: Self = Self(1.0);

    /// Slowest accepted rate; below this a request is almost certainly a typo.
    pub const MIN: Self = Self(0.01);

    /// Fastest accepted rate; beyond this nothing between events is visible.
    pub const MAX: Self = Self(100.0);

    /// Validate a raw rate, or `None` if it is out of range or not finite.
    #[must_use]
    pub fn new(raw: f64) -> Option<Self> {
        // NaN and both infinities fall outside the range.
        (Self::MIN.0..=Self::MAX.0)
            .contains(&raw)
            .then_some(Self(raw))
    }

    /// The rate as a plain multiplier, for display.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl Default for Speed {
    fn default() -> Self {
        Self::NORMAL
    }
}

/// Largest offset produced, in microseconds: `2^53`, the last integer an
/// `f64` represents exactly, so the cast below is exact.
const MAX_OFFSET_MICROS: f64 = 9_007_199_254_740_992.0;

/// When `time_ms` (absolute milliseconds from recording start) is due, as an
/// offset from playback start, at microsecond resolution so `--speed 100`
/// still separates events one millisecond apart.
#[must_use]
pub fn due_at(time_ms: u64, speed: Speed) -> Duration {
    #[allow(
        clippy::cast_precision_loss,
        reason = "only a recording centuries long loses precision; the clamp bounds the result"
    )]
    let micros = time_ms as f64 * 1000.0 / speed.0;
    if !micros.is_finite() || micros <= 0.0 {
        return Duration::ZERO;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "guarded above: finite, positive, and at most 2^53"
    )]
    let whole = micros.min(MAX_OFFSET_MICROS) as u64;
    Duration::from_micros(whole)
}

/// How long one pass over `events` takes at `speed`: the offset of the latest
/// event (`max`, not `last`, since foreign casts need not be sorted).
#[must_use]
pub fn pass_duration(events: &[CastEvent], speed: Speed) -> Duration {
    let latest = events.iter().map(|event| event.time_ms).max().unwrap_or(0);
    due_at(latest, speed)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::cast::EventCode;

    fn speed(raw: f64) -> Speed {
        Speed::new(raw).expect("test speeds are in range")
    }

    #[test]
    fn due_at_divides_wall_clock_time() {
        let cases = [
            (0, 1.0, Duration::ZERO),
            (17_198, 1.0, Duration::from_millis(17_198)),
            (10_000, 2.0, Duration::from_millis(5_000)),
            (10_000, 0.5, Duration::from_secs(20)),
            (10_000, 100.0, Duration::from_millis(100)),
            // Sub-millisecond spacing survives a fast speed.
            (1, 100.0, Duration::from_micros(10)),
            (2, 100.0, Duration::from_micros(20)),
            // A pathological timestamp clamps instead of overflowing.
            (u64::MAX, 0.01, Duration::from_micros(9_007_199_254_740_992)),
        ];
        for (ms, rate, want) in cases {
            assert_eq!(due_at(ms, speed(rate)), want, "{ms} ms at {rate}x");
        }
    }

    #[test]
    fn speed_rejects_the_values_that_would_hang_a_player() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 0.001, 1_000.0] {
            assert!(Speed::new(bad).is_none(), "{bad} accepted");
        }
        assert_eq!(Speed::new(0.01), Some(Speed::MIN));
        assert_eq!(Speed::new(100.0), Some(Speed::MAX));
    }

    #[test]
    fn pass_duration_is_the_latest_events_offset() {
        let events = |times: &[u64]| -> Vec<CastEvent> {
            times
                .iter()
                .map(|ms| CastEvent {
                    time_ms: *ms,
                    code: EventCode::Output,
                    data: String::new(),
                })
                .collect()
        };
        let list = events(&[0, 100, 2_000]);
        assert_eq!(pass_duration(&list, Speed::NORMAL), Duration::from_secs(2));
        assert_eq!(pass_duration(&list, speed(4.0)), Duration::from_millis(500));
        assert_eq!(pass_duration(&[], Speed::NORMAL), Duration::ZERO);
        // Out-of-order casts report their latest event, not their last line.
        let unsorted = events(&[0, 9_000, 1_000]);
        assert_eq!(
            pass_duration(&unsorted, Speed::NORMAL),
            Duration::from_secs(9)
        );
    }
}
