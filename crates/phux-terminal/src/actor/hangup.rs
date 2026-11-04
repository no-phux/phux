//! How a pane's child is hung up at teardown, and how long the reader keeps
//! draining the master while that happens.

use std::path::PathBuf;
use std::time::Duration;

use super::{PANE_KILL_GRACE, PANE_KILL_REAP_BUDGET};

/// How long a gated [`HangupPolicy`] waits for its marker before the grace
/// ceiling begins regardless.
const GATE_WAIT: Duration = Duration::from_secs(30);

/// The grace one actor gives its child after `SIGHUP`. The runtime takes
/// [`Default`]; a test builds an actor with a longer one rather than switching
/// the teardown path.
#[derive(Debug, Clone)]
pub(super) struct HangupPolicy {
    /// A deadline, not a sleep: a child that dies at once returns on the
    /// first poll.
    grace: Duration,
    /// Start the grace only once this file exists.
    gate: Option<PathBuf>,
}

impl Default for HangupPolicy {
    fn default() -> Self {
        Self {
            grace: PANE_KILL_GRACE,
            gate: None,
        }
    }
}

impl HangupPolicy {
    /// A longer `grace`, for a child that needs it under test load.
    #[cfg(test)]
    pub(super) const fn stretched(grace: Duration) -> Self {
        Self { grace, gate: None }
    }

    /// Like [`Self::stretched`], starting the ceiling only once `gate` exists.
    #[cfg(test)]
    pub(super) fn gated(grace: Duration, gate: &std::path::Path) -> Self {
        Self {
            grace,
            gate: Some(gate.to_path_buf()),
        }
    }

    /// The grace ceiling.
    pub(super) const fn grace(&self) -> Duration {
        self.grace
    }

    /// The marker the grace waits for, and how long to wait for it.
    pub(super) fn gate(&self) -> Option<(&std::path::Path, Duration)> {
        self.gate.as_deref().map(|gate| (gate, GATE_WAIT))
    }

    /// Budget for the reader's post-actor drain: the whole window in which
    /// the child may still be writing. It only bounds a child that outlives
    /// the actor and writes forever; EOF ends the drain sooner.
    pub(super) fn orphan_drain_budget(&self) -> Duration {
        let hold = self.gate().map_or(Duration::ZERO, |(_, wait)| wait);
        hold.saturating_add(self.grace)
            .saturating_add(PANE_KILL_REAP_BUDGET)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_drain_budget_covers_every_stage_of_the_hangup() {
        let production = HangupPolicy::default();
        assert_eq!(
            production.orphan_drain_budget(),
            PANE_KILL_GRACE + PANE_KILL_REAP_BUDGET
        );
        let stretched = HangupPolicy::stretched(Duration::from_secs(2));
        assert_eq!(
            stretched.orphan_drain_budget(),
            Duration::from_secs(2) + PANE_KILL_REAP_BUDGET
        );
        let gated = HangupPolicy::gated(Duration::from_secs(2), std::path::Path::new("/marker"));
        assert_eq!(
            gated.orphan_drain_budget(),
            GATE_WAIT + Duration::from_secs(2) + PANE_KILL_REAP_BUDGET
        );
    }
}
