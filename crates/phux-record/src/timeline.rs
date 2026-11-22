//! Idle clamping and marker speed-up on the shared timeline.
//!
//! [`clamp_idle`] runs on the event list before both the `.cast` write and
//! the animation render, so a recording and the GIF derived from it never
//! disagree about how long a pause was. [`compress_marked`] is render-time
//! only: the archival cast keeps the original `m` events and their times.

use crate::cast::{CastEvent, EventCode, secs_to_ms};

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

/// Where an open marked span began, on both clocks.
#[derive(Clone, Copy)]
struct OpenSpan {
    /// The opening marker's original timestamp.
    real_start: u64,
    /// That same instant after earlier spans have been shortened.
    compressed_start: u64,
}

/// Time-compress the spans between marker pairs by `speed`.
///
/// Markers alternate: the first opens a span, the second closes it, the
/// third opens the next. An unpaired trailing marker runs through the last
/// event. Time before the first marker, and between a close and the next
/// open, keeps its duration and shifts earlier by the time already removed.
/// `speed <= 1` or a non-finite speed returns the events unchanged.
///
/// Scaling is integer: a span of `delta` ms becomes
/// `delta * 1000 / round(speed * 1000)`, so a factor of 2 is exact.
#[must_use]
pub fn compress_marked(events: &[CastEvent], speed: f64) -> Vec<CastEvent> {
    let Some((numer, denom)) = speed_ratio(speed) else {
        return events.to_vec();
    };
    let mut out = Vec::with_capacity(events.len());
    let mut shift = 0_u64;
    let mut span: Option<OpenSpan> = None;
    for event in events {
        let original = event.time_ms;
        let compressed = span.map_or_else(
            || original.saturating_sub(shift),
            |open| {
                let delta = original.saturating_sub(open.real_start);
                open.compressed_start
                    .saturating_add(scale_ms(delta, numer, denom))
            },
        );
        if event.code == EventCode::Marker {
            if let Some(open) = span.take() {
                let delta = original.saturating_sub(open.real_start);
                let scaled = scale_ms(delta, numer, denom);
                shift = shift.saturating_add(delta.saturating_sub(scaled));
            } else {
                span = Some(OpenSpan {
                    real_start: original,
                    compressed_start: compressed,
                });
            }
        }
        let mut copied = event.clone();
        copied.time_ms = compressed;
        out.push(copied);
    }
    out
}

/// `1000 / round(speed * 1000)` as a rational, or `None` when speed does
/// not compress.
fn speed_ratio(speed: f64) -> Option<(u64, u64)> {
    if !speed.is_finite() || speed <= 1.0 {
        return None;
    }
    let denom = (speed * 1000.0).round();
    if !denom.is_finite() || denom < 1000.0 || denom >= 9_223_372_036_854_775_808.0 {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the guards above prove the value is finite, at least 1000, and below 2^63"
    )]
    let denom = denom as u64;
    (denom > 0).then_some((1000, denom))
}

/// `delta * numer / denom` in integer arithmetic.
fn scale_ms(delta: u64, numer: u64, denom: u64) -> u64 {
    let product = u128::from(delta).saturating_mul(u128::from(numer));
    u64::try_from(product / u128::from(denom.max(1))).unwrap_or(u64::MAX)
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

    fn ev(time_ms: u64, code: EventCode, data: &str) -> CastEvent {
        CastEvent {
            time_ms,
            code,
            data: data.to_owned(),
        }
    }

    fn times(events: &[CastEvent]) -> Vec<u64> {
        events.iter().map(|event| event.time_ms).collect()
    }

    /// Golden timestamps for `--speed-marked 2`: the span between a marker
    /// pair plays in half the time, and later events shift earlier by the
    /// time removed. A second pair compresses on its own.
    #[test]
    fn golden_speed_marked_two_compresses_each_marker_pair() {
        let events = vec![
            ev(0, EventCode::Output, "a"),
            ev(1_000, EventCode::Marker, "slow"),
            ev(3_000, EventCode::Output, "b"),
            ev(5_000, EventCode::Marker, "slow"),
            ev(7_000, EventCode::Output, "c"),
            ev(8_000, EventCode::Marker, "again"),
            ev(10_000, EventCode::Output, "d"),
        ];
        let got = compress_marked(&events, 2.0);
        assert_eq!(
            times(&got),
            vec![0, 1_000, 2_000, 3_000, 5_000, 6_000, 7_000]
        );
        assert_eq!(got[1].data, "slow");
        assert_eq!(got[1].code, EventCode::Marker);
        assert_eq!(got[5].data, "again");
        // Payloads survive the remap.
        assert_eq!(got[6].data, "d");
    }

    /// An opening marker with no close compresses through the last event.
    #[test]
    fn golden_unclosed_marker_compresses_through_the_end() {
        let events = vec![
            ev(0, EventCode::Output, "a"),
            ev(1_000, EventCode::Marker, "tail"),
            ev(5_000, EventCode::Output, "b"),
        ];
        let got = compress_marked(&events, 2.0);
        assert_eq!(times(&got), vec![0, 1_000, 3_000]);
    }

    #[test]
    fn speed_at_or_below_one_keeps_the_timeline() {
        let events = vec![
            ev(0, EventCode::Output, "a"),
            ev(1_000, EventCode::Marker, "m"),
            ev(3_000, EventCode::Output, "b"),
        ];
        for speed in [1.0, 0.5, f64::NAN, f64::INFINITY] {
            assert_eq!(
                times(&compress_marked(&events, speed)),
                vec![0, 1_000, 3_000],
                "speed {speed}"
            );
        }
    }

    #[test]
    fn a_timeline_with_no_markers_is_unchanged_at_2x() {
        let events = vec![
            ev(0, EventCode::Output, "a"),
            ev(4_000, EventCode::Output, "b"),
        ];
        assert_eq!(times(&compress_marked(&events, 2.0)), vec![0, 4_000]);
    }
}
