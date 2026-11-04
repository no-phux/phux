//! Resolving and applying the pane occupant's identity.

use std::os::fd::RawFd;
use std::time::Instant;

use tracing::trace;

use super::rules::RuleSet;
use super::{
    AgentDetector, DetectOutcome, IDENTIFY_ACQUIRE_POLL, IDENTIFY_ACQUIRE_WINDOW, identify,
    identify_recheck,
};
#[cfg(doc)]
use super::{STARTUP_GRACE, VACANT_CONFIRMATIONS};

/// Who the detector asks about the PTY's foreground process group. A unit
/// test cannot create a real one, so it injects its own.
pub(super) trait IdentitySource {
    /// The cheap half: foreground pgid only.
    fn pgid(&self, master_fd: Option<RawFd>) -> Option<i32>;

    /// The full answer: pgid, argv, and manifest match.
    fn observe(
        &self,
        master_fd: Option<RawFd>,
        pane_child_pid: Option<i32>,
        rules: &RuleSet,
    ) -> (identify::Occupancy, Option<identify::PaneOccupant>);
}

/// The kernel's answer.
pub(super) struct KernelIdentity;

impl IdentitySource for KernelIdentity {
    fn pgid(&self, master_fd: Option<RawFd>) -> Option<i32> {
        identify::foreground_pgid(master_fd)
    }

    fn observe(
        &self,
        master_fd: Option<RawFd>,
        pane_child_pid: Option<i32>,
        rules: &RuleSet,
    ) -> (identify::Occupancy, Option<identify::PaneOccupant>) {
        identify::foreground_observation(master_fd, pane_child_pid, rules)
    }
}

impl AgentDetector {
    /// Identity, cheap first: the periodic full recheck runs as always, and
    /// between rechecks a `tcgetpgrp` mismatch against [`Self::last_pgid`]
    /// triggers an early one. A recycled pgid is invisible here and left to
    /// the full recheck.
    pub(super) fn maybe_identify(
        &mut self,
        now: Instant,
        master_fd: Option<RawFd>,
        pane_child_pid: Option<i32>,
    ) -> Option<DetectOutcome> {
        if now >= self.next_identify {
            return self.reidentify(now, master_fd, pane_child_pid);
        }
        let probed = self.identity.pgid(master_fd);
        if let Some(pgid) = probed
            && Some(pgid) != self.last_pgid
        {
            // Cheap probe disagrees; re-derive now.
            return self.reidentify(now, master_fd, pane_child_pid);
        }
        None
    }

    /// Re-derive identity in full (pgid and argv). `Some` means the identity
    /// step alone resolves the tick.
    fn reidentify(
        &mut self,
        now: Instant,
        master_fd: Option<RawFd>,
        pane_child_pid: Option<i32>,
    ) -> Option<DetectOutcome> {
        let (found, occupant) = self
            .identity
            .observe(master_fd, pane_child_pid, &self.rules);
        if let Some(occupant) = occupant
            && self.published_occupant.as_ref() != Some(&occupant)
        {
            self.pending_occupant = Some(occupant);
        }
        self.apply_identity(now, found)
    }

    /// Take the independently edge-filtered pane-occupant observation.
    pub(crate) const fn take_occupant_update(&mut self) -> Option<identify::PaneOccupant> {
        self.pending_occupant.take()
    }

    /// Advance the occupant edge filter only after the actor enqueues it.
    pub(crate) fn occupant_update_sent(&mut self, occupant: identify::PaneOccupant) {
        self.published_occupant = Some(occupant);
    }

    /// Preserve a failed best-effort enqueue for the next detector tick.
    pub(crate) fn retry_occupant_update(&mut self, occupant: identify::PaneOccupant) {
        self.pending_occupant = Some(occupant);
    }

    /// The manifest's name for `kind`, or the slug itself.
    pub(super) fn manifest_name(&self, kind: &str) -> String {
        self.rules
            .manifest(kind)
            .map_or_else(|| kind.to_owned(), |m| m.name.clone())
    }

    /// Everything after the kernel answered who owns the foreground group.
    /// The transition table, in full:
    ///
    /// | observation | prior identity | action |
    /// |---|---|---|
    /// | `Unresolved` | any | HOLD. Touch nothing. `Quiet`. |
    /// | `Vacant` | none | `Quiet`. |
    /// | `Vacant` | some, streak < [`VACANT_CONFIRMATIONS`] | `Quiet`. |
    /// | `Vacant` | some, streak reached | drop identity, `Retract`. |
    /// | `Agent` | none | acquire; `Quiet` through the startup grace. |
    /// | `Agent`, different kind OR different pgid | some | re-acquire **and** `Reidentified`. |
    /// | `Agent`, same kind, same pgid | some | fall through to the screen. |
    ///
    /// At most one event per tick: `Retract` and `Reidentified` short-circuit,
    /// and the re-anchored [`STARTUP_GRACE`] keeps a new occupant's `State`
    /// strictly after its correction.
    fn apply_identity(
        &mut self,
        now: Instant,
        occupancy: identify::Occupancy,
    ) -> Option<DetectOutcome> {
        use identify::Occupancy;

        // Remember this full resolution's pgid for the cheap probe
        // (`Unresolved` carries none).
        if let Some(pgid) = occupancy.pgid() {
            self.last_pgid = Some(pgid);
        }

        let acquiring = !matches!(occupancy, Occupancy::Agent { .. })
            && now < self.started + IDENTIFY_ACQUIRE_WINDOW;
        self.next_identify = now
            + if acquiring {
                IDENTIFY_ACQUIRE_POLL
            } else {
                identify_recheck()
            };

        match occupancy {
            // A failed query is not an observation: hold, and never advance
            // the vacancy streak.
            Occupancy::Unresolved => {
                trace!("agent-detect: occupancy unresolved; holding");
                Some(DetectOutcome::Quiet)
            }
            Occupancy::Vacant { .. } => Some(self.apply_vacancy()),
            Occupancy::Agent { kind, occupant } => self.apply_agent(now, kind, occupant),
        }
    }
}
