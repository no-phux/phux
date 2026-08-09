//! Timebase arithmetic shared by the codec and the renderer.
//!
//! There is exactly one timeline in this crate — absolute integer
//! milliseconds from session start — and one transformation applied to it:
//! idle clamping. [`clamp_idle`] runs on the *shared* event list, before both
//! the `.cast` write and the animation render, so a recording and the GIF
//! derived from it can never disagree about how long a pause was.
//!
//! Doing it the other way round (clamping only in the renderer) is the
//! obvious shortcut and it is wrong: the archival `.cast` would then carry a
//! 40-second coffee break that the GIF collapsed to 2 seconds, and every
//! marker or timestamp a user reads off one artifact would mislead them about
//! the other.

use crate::cast::{CastEvent, secs_to_ms};

/// Collapse every idle gap longer than `limit_secs` down to `limit_secs`.
///
/// Walks the absolute timeline once; whenever `t[i] - t[i-1]` exceeds the
/// limit, the excess is subtracted from `t[i..]`. Event *order* and every
/// sub-limit gap are preserved exactly — only the long pauses shrink.
///
/// `None`, a non-finite limit, or a limit `<= 0.0` disables the clamp.
pub fn clamp_idle(events: &mut [CastEvent], limit_secs: Option<f64>) {
    let Some(limit) = limit_secs else {
        return;
    };
    if !limit.is_finite() || limit <= 0.0 {
        return;
    }
    let limit_ms = secs_to_ms(limit);
    if limit_ms == 0 {
        return;
    }

    // `shift` is the total time removed so far. Subtracting a running total
    // (rather than rewriting the tail on every gap) keeps this O(n) and makes
    // the monotonicity of the result obvious.
    let mut shift = 0_u64;
    let mut previous = 0_u64;
    for event in events.iter_mut() {
        let original = event.time_ms;
        let gap = original.saturating_sub(previous);
        if gap > limit_ms {
            shift = shift.saturating_add(gap - limit_ms);
        }
        previous = original;
        event.time_ms = original.saturating_sub(shift);
    }
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
            .enumerate()
            .map(|(idx, ms)| CastEvent {
                time_ms: *ms,
                code: EventCode::Output,
                data: idx.to_string(),
            })
            .collect()
    }

    fn times(events: &[CastEvent]) -> Vec<u64> {
        events.iter().map(|event| event.time_ms).collect()
    }

    #[test]
    fn clamp_idle_shifts_all_later_events() {
        // 0 -> 100 (fine), 100 -> 10_100 (a 10 s pause, clamped to 2 s),
        // 10_100 -> 10_200 (fine, and must stay 100 ms apart afterwards).
        let mut list = events(&[0, 100, 10_100, 10_200]);
        clamp_idle(&mut list, Some(2.0));
        assert_eq!(times(&list), [0, 100, 2100, 2200]);
    }

    #[test]
    fn clamp_idle_none_is_a_noop() {
        let mut list = events(&[0, 100, 60_000]);
        clamp_idle(&mut list, None);
        assert_eq!(times(&list), [0, 100, 60_000]);

        let mut list = events(&[0, 100, 60_000]);
        clamp_idle(&mut list, Some(0.0));
        assert_eq!(times(&list), [0, 100, 60_000]);

        let mut list = events(&[0, 100, 60_000]);
        clamp_idle(&mut list, Some(-1.0));
        assert_eq!(times(&list), [0, 100, 60_000]);
    }

    #[test]
    fn clamp_idle_preserves_event_order() {
        let mut list = events(&[0, 5_000, 5_010, 90_000, 90_001]);
        clamp_idle(&mut list, Some(1.5));
        let out = times(&list);
        assert!(out.windows(2).all(|pair| pair[0] <= pair[1]), "{out:?}");
        // The payloads must still be in their original sequence.
        assert_eq!(
            list.iter().map(|e| e.data.as_str()).collect::<Vec<_>>(),
            ["0", "1", "2", "3", "4"]
        );
        assert_eq!(out, [0, 1500, 1510, 3010, 3011]);
    }

    #[test]
    fn clamp_idle_leaves_gaps_under_the_limit_alone() {
        let mut list = events(&[0, 1000, 1900, 2500]);
        clamp_idle(&mut list, Some(2.0));
        assert_eq!(times(&list), [0, 1000, 1900, 2500]);
    }

    #[test]
    fn clamp_idle_handles_an_empty_list() {
        let mut list: Vec<CastEvent> = Vec::new();
        clamp_idle(&mut list, Some(2.0));
        assert!(list.is_empty());
    }
}
