//! Window-level retry scheduling; transport reconnect remains runtime-owned.

use super::{Desk, ENSURE_AFTER, ENSURE_EVERY, Instant, Tx, backoff};

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
