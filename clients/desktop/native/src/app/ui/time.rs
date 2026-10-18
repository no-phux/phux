//! Relative ages for rows and tooltips.

/// Seconds, minutes, hours or days since `then`, both in Unix milliseconds.
pub(crate) fn ago(then: u64, now: u64) -> String {
    let seconds = now.saturating_sub(then) / 1000;
    match seconds {
        0..5 => "now".into(),
        5..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", (seconds + 30) / 60),
        3600..86400 => format!("{}h", (seconds + 1800) / 3600),
        _ => format!("{}d", (seconds + 43200) / 86400),
    }
}

/// Milliseconds from `now` until [`ago`]'s label for `then` next changes.
pub(crate) fn ago_wait(then: u64, now: u64) -> u64 {
    let elapsed = now.saturating_sub(then);
    let seconds = elapsed / 1000;
    // Each bucket rounds to its nearest unit, so its label turns at the half unit.
    let turn = |unit: u64| ((seconds + unit / 2) / unit + 1) * unit - unit / 2;
    let next = match seconds {
        0..5 => 5,
        5..60 => seconds + 1,
        60..3600 => turn(60).min(3600),
        3600..86400 => turn(3600).min(86400),
        _ => turn(86400),
    };
    next * 1000 - elapsed
}

#[cfg(test)]
mod tests {
    use super::{ago, ago_wait};

    #[test]
    fn the_wait_ends_exactly_when_the_label_changes() {
        let mut now = 0;
        while now < 4 * 86_400_000 {
            let wait = ago_wait(0, now);
            assert!(wait > 0);
            assert_eq!(ago(0, now), ago(0, now + wait - 1), "at {now}");
            assert_ne!(ago(0, now), ago(0, now + wait), "at {now}");
            now += wait;
        }
    }

    #[test]
    fn minutes_and_hours_sleep_past_seconds() {
        assert_eq!(ago_wait(0, 3_000), 2_000);
        assert_eq!(ago_wait(0, 10_500), 500);
        assert_eq!(ago_wait(0, 120_000), 30_000);
        assert_eq!(ago_wait(0, 2 * 3_600_000), 1_800_000);
    }
}
