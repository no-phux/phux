//! Restart delay and the give-up window.
//!
//! Backoff is 1, 2, 4, 8, 16, then 30 seconds, and it returns to 1 second
//! after the process has been up for at least 60 seconds. Give-up is
//! separate: the sixth failure inside ten minutes is not restarted.

use std::time::Duration;

/// Delays before successive restarts. The last value repeats.
const BACKOFFS: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
    Duration::from_secs(30),
];

/// Uptime after which the next failure restarts with the first delay.
const STABLE_UPTIME: Duration = Duration::from_secs(60);

/// How far back a failure still counts toward give-up.
const GIVE_UP_WINDOW: Duration = Duration::from_mins(10);

/// Failures allowed inside [`GIVE_UP_WINDOW`] before the next one gives up.
/// The sixth failure in the window is the one that stays dead.
const FAILURES_BEFORE_GIVE_UP: usize = 5;

/// What to do after one sidecar exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AfterExit {
    /// Exit 0. Do not restart.
    Clean,
    /// Non-zero or signalled. Restart after `delay`.
    Restart { delay: Duration },
    /// Too many failures in the window. Leave it dead.
    GiveUp,
}

/// Per-sidecar restart memory. Times are durations from an arbitrary origin
/// so tests can drive the window without sleeping.
#[derive(Debug, Clone)]
pub(super) struct Policy {
    backoff_index: usize,
    /// When each counted failure happened, as an offset from the origin.
    failures: Vec<Duration>,
}

impl Policy {
    pub(super) const fn new() -> Self {
        Self {
            backoff_index: 0,
            failures: Vec::new(),
        }
    }

    /// `uptime` is how long this process lived. `now` is the same clock the
    /// failure marks use.
    pub(super) fn on_exit(
        &mut self,
        code: Option<i32>,
        uptime: Duration,
        now: Duration,
    ) -> AfterExit {
        if code == Some(0) {
            return AfterExit::Clean;
        }
        self.failures.push(now);
        self.failures
            .retain(|at| now.saturating_sub(*at) <= GIVE_UP_WINDOW);
        if self.failures.len() > FAILURES_BEFORE_GIVE_UP {
            return AfterExit::GiveUp;
        }
        if uptime >= STABLE_UPTIME {
            self.backoff_index = 0;
        }
        let delay = BACKOFFS[self.backoff_index.min(BACKOFFS.len() - 1)];
        if self.backoff_index + 1 < BACKOFFS.len() {
            self.backoff_index += 1;
        }
        AfterExit::Restart { delay }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn fail(policy: &mut Policy, uptime: Duration, now: Duration) -> AfterExit {
        policy.on_exit(Some(1), uptime, now)
    }

    #[test]
    fn backoff_steps_then_holds_at_thirty_seconds() {
        let mut policy = Policy::new();
        let mut now = Duration::ZERO;
        for expected in [1_u64, 2, 4, 8, 16, 30, 30] {
            // Spread failures so the ten-minute window never fills.
            now += Duration::from_mins(3);
            assert_eq!(
                fail(&mut policy, Duration::from_secs(1), now),
                AfterExit::Restart {
                    delay: Duration::from_secs(expected)
                }
            );
        }
    }

    #[test]
    fn sixth_failure_inside_the_window_gives_up() {
        let mut policy = Policy::new();
        for n in 0..5 {
            assert!(matches!(
                fail(&mut policy, Duration::ZERO, Duration::from_secs(n)),
                AfterExit::Restart { .. }
            ));
        }
        assert_eq!(
            fail(&mut policy, Duration::ZERO, Duration::from_secs(5)),
            AfterExit::GiveUp
        );
    }

    #[test]
    fn failures_older_than_ten_minutes_leave_the_window() {
        let mut policy = Policy::new();
        for n in 0..5 {
            let _ = fail(&mut policy, Duration::ZERO, Duration::from_secs(n));
        }
        let later = Duration::from_secs(10 * 60 + 30);
        assert!(matches!(
            fail(&mut policy, Duration::ZERO, later),
            AfterExit::Restart { .. }
        ));
    }

    #[test]
    fn stable_uptime_resets_backoff_without_clearing_the_window() {
        let mut policy = Policy::new();
        assert_eq!(
            fail(&mut policy, Duration::ZERO, Duration::from_secs(1)),
            AfterExit::Restart {
                delay: Duration::from_secs(1)
            }
        );
        assert_eq!(
            fail(
                &mut policy,
                Duration::from_secs(60),
                Duration::from_secs(70)
            ),
            AfterExit::Restart {
                delay: Duration::from_secs(1)
            }
        );
        // The window still holds both failures; a burst can still give up.
        for n in 0..3 {
            let _ = fail(&mut policy, Duration::ZERO, Duration::from_secs(71 + n));
        }
        assert_eq!(
            fail(&mut policy, Duration::ZERO, Duration::from_secs(80)),
            AfterExit::GiveUp
        );
    }

    #[test]
    fn exit_zero_does_not_restart_or_count() {
        let mut policy = Policy::new();
        assert_eq!(
            policy.on_exit(Some(0), Duration::from_secs(1), Duration::from_secs(1)),
            AfterExit::Clean
        );
        assert!(policy.failures.is_empty());
        assert_eq!(policy.backoff_index, 0);
    }
}
