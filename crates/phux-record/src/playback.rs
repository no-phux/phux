//! Playback scheduling: turning a recorded timeline into wall-clock
//! deadlines.
//!
//! This is the arithmetic half of `phux play` (ADR-0064). It knows nothing
//! about panes, PTYs, or the wire — it answers one question, *when is this
//! event due?*, so the answer can be unit-tested without a server, a clock,
//! or a terminal.
//!
//! # Absolute offsets, never per-event deltas
//!
//! [`due_at`] returns the offset from **playback start**, not from the
//! previous event. That is the whole point of the module. A driver that
//! slept `t[i] - t[i-1]` between events would add the scheduler's wakeup
//! error to every gap and accumulate it: at 60 events per second, a
//! systematic 1 ms overshoot is 3.6 seconds of drift per recorded minute,
//! and the recording would finish visibly later than it was captured. A
//! driver that sleeps *until* `start + due_at(t[i])` cannot drift at all,
//! because every deadline is recomputed from the same anchor. Late wakeups
//! are absorbed instead of compounded.
//!
//! The same reasoning is why the offsets stay in integer microseconds:
//! [`crate::cast::CastEvent::time_ms`] is integer milliseconds precisely so
//! nothing in this crate accumulates float error, and a scheduler that
//! re-derived seconds as `f64` per step would put it back.
//!
//! # Speed is a divisor of time, not a multiplier of rate
//!
//! `--speed 2` means "half the wall-clock", i.e. every deadline is divided
//! by two. It does **not** resample, drop, or merge events: playback at any
//! speed emits exactly the events the cast holds, in order, with the same
//! relative spacing. A recording is not a video and there is no frame rate
//! to change.

use std::time::Duration;

use crate::cast::CastEvent;

/// A validated playback rate: strictly positive, finite, and bounded.
///
/// A newtype rather than a bare `f64` because every arithmetic site in this
/// module divides by it. `0.0` would produce an infinite deadline, a
/// negative would produce one in the past, and `NaN` would produce a
/// comparison that is false in both directions — three different ways for a
/// player to hang or to flush an entire recording in one tick. Validating
/// once at the parse boundary means none of the call sites below has to
/// carry a guard.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Speed(f64);

impl Speed {
    /// Real time: one recorded second takes one wall-clock second.
    pub const NORMAL: Self = Self(1.0);

    /// Slowest accepted rate.
    ///
    /// A hundredth of real time stretches a one-minute recording to an hour
    /// and 40 minutes. Below that the request is almost certainly a typo
    /// (`--speed 0.001` for `--speed 0.01`), and the failure mode of
    /// honoring it — a pane that appears frozen for days — is far worse
    /// than a rejection that names the bound.
    pub const MIN: Self = Self(0.01);

    /// Fastest accepted rate.
    ///
    /// A hundred times real time collapses a ten-minute recording to six
    /// seconds, which is past the point where anything is visible and
    /// already below the granularity of the deadlines being computed. Faster
    /// than this is not "faster playback", it is "paint the final frame",
    /// and the honest way to ask for that is to snapshot the last frame.
    pub const MAX: Self = Self(100.0);

    /// Validate a raw rate, or `None` if it is not one this module can
    /// divide by.
    ///
    /// Deliberately returns `None` rather than a typed error: the only
    /// caller is a CLI argument parser, which owns the wording of its own
    /// diagnostic, and a `RecordError` variant here would be a vocabulary
    /// nobody reads.
    #[must_use]
    pub fn new(raw: f64) -> Option<Self> {
        // `contains` covers the non-finite cases for free and is the reason
        // there is no separate `is_finite` guard: every comparison against
        // NaN is false, so NaN is outside every range, and both infinities
        // are outside this one.
        if !(Self::MIN.0..=Self::MAX.0).contains(&raw) {
            return None;
        }
        Some(Self(raw))
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

/// Largest offset this module will produce, in microseconds.
///
/// `2^53` is the last integer an `f64` represents exactly, so clamping the
/// scaled value there keeps the one lossy cast below provably exact rather
/// than merely probably exact. It is also ~285 years, which no recording
/// reaches — and a `time_ms` that large can only have come from the
/// saturating arithmetic in [`crate::timeline::clamp_idle`], never from a
/// real capture.
const MAX_OFFSET_MICROS: f64 = 9_007_199_254_740_992.0;

/// When `time_ms` (absolute milliseconds from the recording's start) is due,
/// as an offset from playback start.
///
/// Deadlines are microseconds so that the arithmetic has a thousand times
/// the resolution of its input and `--speed 100` still separates events that
/// were one millisecond apart.
#[must_use]
pub fn due_at(time_ms: u64, speed: Speed) -> Duration {
    // Guards first, cast last — the same discipline as `timeline::secs_to_ms`,
    // so the lint allow below covers a value that has already been proven to
    // be finite, non-negative, and in range instead of hiding a real bug.
    #[allow(
        clippy::cast_precision_loss,
        reason = "a recording long enough to lose milliseconds here would be 285 years; the clamp below bounds the result exactly"
    )]
    let micros = time_ms as f64 * 1000.0 / speed.0;
    if !micros.is_finite() || micros <= 0.0 {
        return Duration::ZERO;
    }
    let bounded = micros.min(MAX_OFFSET_MICROS);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "guarded above: finite, positive, and at most 2^53"
    )]
    let whole = bounded as u64;
    Duration::from_micros(whole)
}

/// How long one pass over `events` takes at `speed`.
///
/// Reported by `phux play` before the pane is even created, so a caller
/// learns "this is a four-minute recording and you asked for real time"
/// without waiting four minutes to find out.
///
/// The last event's own duration is unknowable — a cast records when bytes
/// arrived, never how long the frame they painted was meant to linger — so
/// this is the offset of the last event, not one tick past it.
#[must_use]
pub fn pass_duration(events: &[CastEvent], speed: Speed) -> Duration {
    // `max` rather than `last`: the timebase is monotonic non-decreasing by
    // construction on write and after `clamp_idle`, but this function is
    // also handed casts phux did not produce, and a hand-edited file with
    // one out-of-order line should report the honest total rather than a
    // shorter one.
    let latest = events.iter().map(|event| event.time_ms).max().unwrap_or(0);
    due_at(latest, speed)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;
    use crate::cast::EventCode;

    fn events(times: &[u64]) -> Vec<CastEvent> {
        times
            .iter()
            .map(|ms| CastEvent {
                time_ms: *ms,
                code: EventCode::Output,
                data: String::from("x"),
            })
            .collect()
    }

    fn speed(raw: f64) -> Speed {
        Speed::new(raw).expect("test speeds are in range")
    }

    #[test]
    fn real_time_is_the_identity() {
        assert_eq!(due_at(0, Speed::NORMAL), Duration::ZERO);
        assert_eq!(due_at(1, Speed::NORMAL), Duration::from_millis(1));
        assert_eq!(due_at(17_198, Speed::NORMAL), Duration::from_millis(17_198));
    }

    #[test]
    fn speed_divides_wall_clock_time() {
        // 2x is half the wall clock; 0.5x is double it. Stated as the two
        // directions because an off-by-inversion here is the single most
        // likely bug in the module and it is silent: playback still works,
        // it is just wrong in a way only a stopwatch catches.
        assert_eq!(due_at(10_000, speed(2.0)), Duration::from_millis(5_000));
        assert_eq!(due_at(10_000, speed(0.5)), Duration::from_millis(20_000));
        assert_eq!(due_at(10_000, speed(100.0)), Duration::from_millis(100));
    }

    #[test]
    fn sub_millisecond_spacing_survives_a_fast_speed() {
        // At 100x, two events 1 ms apart are 10 us apart. A scheduler that
        // rounded to milliseconds would collapse them onto the same deadline
        // and lose the ordering the cast recorded.
        let first = due_at(1, speed(100.0));
        let second = due_at(2, speed(100.0));
        assert_eq!(first, Duration::from_micros(10));
        assert_eq!(second, Duration::from_micros(20));
        assert!(first < second);
    }

    #[test]
    fn offsets_are_absolute_so_they_never_accumulate() {
        // The property the module exists for: the offset of event N is a
        // pure function of event N's own timestamp. Summing the deltas must
        // reproduce it exactly, at any speed, for any number of events.
        let times: Vec<u64> = (0..1_000).map(|i| i * 17).collect();
        let rate = speed(3.0);
        let mut summed = Duration::ZERO;
        let mut previous = 0_u64;
        for time in &times {
            summed += due_at(*time, rate) - due_at(previous, rate);
            previous = *time;
        }
        assert_eq!(summed, due_at(*times.last().unwrap(), rate));
    }

    #[test]
    fn a_leading_pause_is_honored_not_trimmed() {
        // A cast whose first event is at t=5s opens with a five-second hold.
        // That is the recording's own shape; collapsing it is `--idle-limit`'s
        // job (`timeline::clamp_idle`), not the scheduler's, and doing it in
        // both places would clamp twice.
        assert_eq!(due_at(5_000, Speed::NORMAL), Duration::from_secs(5));
    }

    #[test]
    fn speed_rejects_the_values_that_would_hang_a_player() {
        assert!(
            Speed::new(0.0).is_none(),
            "zero divides to an infinite wait"
        );
        assert!(
            Speed::new(-1.0).is_none(),
            "negative is a deadline in the past"
        );
        assert!(
            Speed::new(f64::NAN).is_none(),
            "NaN compares false both ways"
        );
        assert!(Speed::new(f64::INFINITY).is_none());
        assert!(Speed::new(0.001).is_none(), "below MIN");
        assert!(Speed::new(1_000.0).is_none(), "above MAX");

        // The bounds themselves are accepted: a rejection at exactly the
        // documented limit is the kind of off-by-one that makes a help
        // string a lie.
        assert_eq!(Speed::new(0.01), Some(Speed::MIN));
        assert_eq!(Speed::new(100.0), Some(Speed::MAX));
        assert_eq!(Speed::new(1.0), Some(Speed::NORMAL));
    }

    #[test]
    fn a_pathological_timestamp_clamps_instead_of_overflowing() {
        // `clamp_idle` saturates to u64::MAX on a malformed file. Scaling
        // that must produce a large Duration, not a panic and not a wrapped
        // tiny one that would flush the recording instantly.
        let huge = due_at(u64::MAX, speed(0.01));
        assert_eq!(huge, Duration::from_micros(9_007_199_254_740_992));
    }

    #[test]
    fn pass_duration_is_the_last_events_offset() {
        let list = events(&[0, 100, 2_000]);
        assert_eq!(pass_duration(&list, Speed::NORMAL), Duration::from_secs(2));
        assert_eq!(pass_duration(&list, speed(4.0)), Duration::from_millis(500));
        assert_eq!(pass_duration(&[], Speed::NORMAL), Duration::ZERO);
    }

    #[test]
    fn pass_duration_reports_the_latest_event_of_an_out_of_order_cast() {
        // Hand-edited or third-party casts are not required to be sorted.
        // Reporting 1s for a file that ends at 9s would understate the wait
        // by nine-fold.
        let list = events(&[0, 9_000, 1_000]);
        assert_eq!(pass_duration(&list, Speed::NORMAL), Duration::from_secs(9));
    }
}
