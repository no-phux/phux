//! When a window or display mirror captures next. Idle, it captures every
//! `--interval`; after a viewer's input it captures [`SETTLE`] later, then
//! every [`ACTIVE`] for [`ACTIVE_FOR`], so the input's effect shows without
//! waiting out the interval.
//!
//! The fast interval runs from the end of the last capture, so however slow
//! a capture is, the mirror rests at least [`ACTIVE`] between two.

use std::time::{Duration, Instant};

/// How long the app gets to repaint before the capture that should show an
/// input's effect.
pub(super) const SETTLE: Duration = Duration::from_millis(50);
/// The interval while input is recent.
pub(super) const ACTIVE: Duration = Duration::from_millis(150);
/// How long after the last input the mirror keeps the fast interval.
pub(super) const ACTIVE_FOR: Duration = Duration::from_secs(2);

/// The longest a mirror waits between captures that yield no frame.
pub(super) const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// The idle interval after `misses` captures in a row gave no frame (the
/// display asleep, the session locked): doubling from `idle` up to
/// [`MAX_BACKOFF`], or `idle` itself once a capture works.
pub(super) fn backoff(idle: Duration, misses: u32) -> Duration {
    idle.saturating_mul(1 << misses.min(8))
        .min(MAX_BACKOFF)
        .max(idle)
}

/// When to capture next: `captured` is when the last capture ended, `input`
/// when an intent was last acted on, `idle` the `--interval`.
pub(super) fn next_capture(captured: Instant, input: Option<Instant>, idle: Duration) -> Instant {
    let idle_at = captured + idle;
    match input {
        Some(input) if input >= captured => idle_at.min(input + SETTLE),
        Some(input) if captured < input + ACTIVE_FOR => idle_at.min(captured + ACTIVE),
        _ => idle_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: Duration = Duration::from_secs(1);
    const fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn idle_captures_wait_out_the_interval() {
        let t = Instant::now();
        assert_eq!(next_capture(t, None, IDLE), t + IDLE);
        // Input long before the last capture no longer counts.
        assert_eq!(next_capture(t + ms(5000), Some(t), IDLE), t + ms(6000));
    }

    #[test]
    fn input_brings_the_next_capture_forward_and_keeps_it_fast_for_a_while() {
        let t = Instant::now();
        // Input after the last capture: capture once the app has repainted.
        assert_eq!(
            next_capture(t, Some(t + ms(300)), IDLE),
            t + ms(300) + SETTLE
        );
        // Captures since the input come every ACTIVE until ACTIVE_FOR passes.
        let input = t + ms(300);
        let captured = input + ms(400);
        assert_eq!(next_capture(captured, Some(input), IDLE), captured + ACTIVE);
        let late = input + ACTIVE_FOR;
        assert_eq!(next_capture(late, Some(input), IDLE), late + IDLE);
    }

    #[test]
    fn failed_captures_back_off_to_a_ceiling_and_recover_at_once() {
        assert_eq!(backoff(IDLE, 0), IDLE);
        assert_eq!(backoff(IDLE, 1), ms(2000));
        assert_eq!(backoff(IDLE, 3), ms(8000));
        assert_eq!(backoff(IDLE, 40), MAX_BACKOFF);
        // An interval already past the ceiling is never shortened.
        assert_eq!(backoff(ms(60_000), 2), ms(60_000));
    }

    #[test]
    fn a_shorter_idle_interval_is_never_slowed_down() {
        let t = Instant::now();
        let idle = ms(100);
        assert_eq!(next_capture(t, Some(t + ms(90)), idle), t + idle);
        assert_eq!(next_capture(t + ms(10), Some(t), idle), t + ms(110));
    }
}
