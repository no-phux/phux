//! Idle clamping on the shared timeline.
//!
//! [`clamp_idle`] runs on the event list before both the `.cast` write and
//! the animation render, so a recording and the GIF derived from it never
//! disagree about how long a pause was.

use crate::cast::{CastEvent, secs_to_ms};

/// Collapse every idle gap longer than `limit_secs` down to `limit_secs`.
///
/// Event order and every sub-limit gap are preserved exactly. `None`, a
/// non-finite limit, or a limit `<= 0.0` disables the clamp.
pub fn clamp_idle(events: &mut [CastEvent], limit_secs: Option<f64>) {
    let Some(limit) = limit_secs else {
        return;
    };
    let limit_ms = secs_to_ms(limit);
    if limit_ms == 0 {
        return;
    }

    // `shift` is the total time removed so far.
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
mod tests {
    use super::*;
    use crate::cast::EventCode;

    #[test]
    fn clamp_idle_table() {
        let cases: &[(&[u64], Option<f64>, &[u64])] = &[
            // A 10 s pause clamps to 2 s; later gaps are preserved.
            (&[0, 100, 10_100, 10_200], Some(2.0), &[0, 100, 2100, 2200]),
            (
                &[0, 5_000, 5_010, 90_000, 90_001],
                Some(1.5),
                &[0, 1500, 1510, 3010, 3011],
            ),
            (&[0, 1000, 1900, 2500], Some(2.0), &[0, 1000, 1900, 2500]),
            (&[0, 100, 60_000], None, &[0, 100, 60_000]),
            (&[0, 100, 60_000], Some(0.0), &[0, 100, 60_000]),
            (&[0, 100, 60_000], Some(-1.0), &[0, 100, 60_000]),
            (&[0, 100, 60_000], Some(f64::NAN), &[0, 100, 60_000]),
            (&[], Some(2.0), &[]),
        ];
        for (input, limit, want) in cases {
            let mut list: Vec<CastEvent> = input
                .iter()
                .map(|ms| CastEvent {
                    time_ms: *ms,
                    code: EventCode::Output,
                    data: String::new(),
                })
                .collect();
            clamp_idle(&mut list, *limit);
            let got: Vec<u64> = list.iter().map(|event| event.time_ms).collect();
            assert_eq!(got, *want, "input {input:?} limit {limit:?}");
        }
    }
}
