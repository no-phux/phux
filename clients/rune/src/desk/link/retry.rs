//! Window-level retry scheduling; transport reconnect remains runtime-owned.

use super::{Desk, ENSURE_AFTER, ENSURE_EVERY, Instant, Tx};
use std::time::Duration;

/// The longest wait between automatic attempts on an ended connection.
const RETRY_CEILING: Duration = Duration::from_secs(30);
/// The wait before automatic attempt `tries`: 2 s doubling to the ceiling.
fn backoff(tries: u32) -> Duration {
    Duration::from_secs(2u64.saturating_mul(1u64 << tries.min(8))).min(RETRY_CEILING)
}
impl Desk {
    /// An ended connection: a new one on a widening schedule.
    pub(super) fn tend_dead(&mut self, now: Instant, tx: &mut Tx) -> bool {
        if self.stuck() {
            return false;
        }
        let Some(at) = self.link.retry_at else {
            let wait = backoff(self.link.tries);
            self.link.retry_at = Some(now + wait);
            tx.wake_after(wait);
            return false;
        };
        if now < at {
            return false;
        }
        self.link.retry_at = None;
        self.link.tries += 1;
        self.attempt();
        true
    }

    /// A live client walking its ladder, whose every step wakes this: a
    /// local server may need starting.
    pub(super) fn tend_dialing(&mut self) {
        let local = self.machines.current.is_none() && self.launch.can_ensure();
        let since = self.link.last_ensure.map(|at| at.elapsed());
        let due = since.is_none_or(|since| since >= ENSURE_EVERY);
        if local && due && self.link.is_down_for(ENSURE_AFTER) {
            self.find_server();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
#[test]
fn retries_widen_to_a_ceiling() {
    assert_eq!(backoff(0), Duration::from_secs(2));
    assert_eq!(backoff(1), Duration::from_secs(4));
    assert_eq!(backoff(3), Duration::from_secs(16));
    assert_eq!(backoff(9), RETRY_CEILING);
    assert_eq!(backoff(u32::MAX), RETRY_CEILING);
}
}
