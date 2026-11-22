//! Publishing vacancy and agent state, including the idle hold.

#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

use tracing::trace;

use super::{
    AgentDetector, DetectOutcome, DetectedState, IDLE_CONFIRMATIONS, IDLE_HOLD_CAP, Identified,
    PendingIdle, VACANT_CONFIRMATIONS, identify,
};

impl AgentDetector {
    /// A *successful* observation that no known agent owns the pane.
    pub(super) fn apply_vacancy(&mut self) -> DetectOutcome {
        if self.identified.is_none() {
            self.vacant_streak = 0;
            return DetectOutcome::Quiet;
        }
        self.vacant_streak = self.vacant_streak.saturating_add(1);
        if self.vacant_streak < VACANT_CONFIRMATIONS {
            trace!(
                streak = self.vacant_streak,
                "agent-detect: vacant, not yet confirmed",
            );
            return DetectOutcome::Quiet;
        }

        // Confirmed gone: retract.
        self.identified = None;
        self.pending_idle = None;
        self.current = None;
        self.pending_hook = None;
        self.vacant_streak = 0;
        // Gate on having had an identity, not on `published`, which may be
        // briefly `None` after any metadata write.
        self.published = None;
        trace!("agent-detect: occupant confirmed gone; retracting");
        DetectOutcome::Retract
    }

    /// A *successful* observation that `kind` (running as `occupant`) owns the
    /// pane.
    pub(super) fn apply_agent(
        &mut self,
        now: Instant,
        kind: String,
        occupant: identify::Occupant,
    ) -> Option<DetectOutcome> {
        self.vacant_streak = 0;

        if let Some(agent) = self.identified.as_mut()
            && agent.kind == kind
            // A `None` prior occupant heals silently rather than counting as
            // a change.
            && agent.occupant.is_none_or(|prior| prior.same(occupant))
        {
            // Refresh, but never downgrade a known start time to `None`.
            agent.occupant = Some(match agent.occupant {
                Some(prior) if occupant.started.is_none() => {
                    identify::Occupant::new(occupant.pgid, prior.started)
                }
                _ => occupant,
            });
            return None;
        }

        let replaced = self.identified.is_some();
        trace!(%kind, pgid = occupant.pgid, replaced, "agent-detect: identified");
        // Grace restarts here, including for a same-kind restart.
        self.identified = Some(Identified {
            kind: kind.clone(),
            occupant: Some(occupant),
            at: now,
        });
        // A different occupant: nothing previously derived applies.
        self.published = None;
        self.pending_idle = None;
        self.current = None;

        // Acquiring an empty pane is not a correction.
        if !replaced {
            return None;
        }
        let name = self.manifest_name(&kind);
        Some(DetectOutcome::Reidentified { kind, name })
    }

    /// The `working -> idle` hold. Returns whether `Idle` may be published now.
    pub(super) fn settle_idle(&mut self, now: Instant, visible_idle: bool) -> bool {
        let releasing_work = self
            .published
            .as_ref()
            .is_some_and(|r| r.state == DetectedState::Working);

        // Positive idle evidence, or we were not holding a `working` badge in
        // the first place: nothing to debounce.
        if visible_idle || !releasing_work {
            self.pending_idle = None;
            return true;
        }

        // Ambiguous: look again, fast (the open hold sets the cadence).
        let pending = self.pending_idle.get_or_insert(PendingIdle {
            confirmations: 0,
            since: now,
        });
        pending.confirmations = pending.confirmations.saturating_add(1);
        let settled = pending.confirmations >= IDLE_CONFIRMATIONS
            || now.duration_since(pending.since) >= IDLE_HOLD_CAP;
        if settled {
            self.pending_idle = None;
        }
        settled
    }

    /// Test seam: the state this detector last published.
    #[cfg(test)]
    pub(crate) fn published_state(&self) -> Option<DetectedState> {
        self.published.as_ref().map(|report| report.state)
    }
    #[cfg(test)]
    /// Test seam: force an identity without a live PTY.
    #[cfg(test)]
    pub(crate) fn force_identity(&mut self, kind: &str, now: Instant) {
        self.identified = Some(Identified {
            kind: kind.to_owned(),
            occupant: self.identified.take().and_then(|agent| agent.occupant),
            at: now,
        });
        // Push the next identity poll out; there is no PTY to read.
        self.next_identify = now + Duration::from_secs(3600);
    }
}
