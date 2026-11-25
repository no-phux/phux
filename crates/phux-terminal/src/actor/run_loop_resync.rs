//! One bounded resize/gap debounce and its exact publication audience.

use super::{RESIZE_RESYNC_DEBOUNCE, ResyncAudience, ResyncReason, ResyncTarget};

/// One claim on the debounced resync: why, and for a gap, which pump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::actor) struct OwedResync {
    /// Why the requester's generation cannot continue.
    pub(in crate::actor) reason: ResyncReason,
    /// The pump that fell behind; `None` means every subscriber.
    pub(in crate::actor) target: Option<ResyncTarget>,
}

/// The resync a debounce window owes: why, and to whom.
struct Owed {
    /// Why the resync was armed; broadcast when the deadline fires.
    reason: ResyncReason,
    /// Every subscriber is owed the resync, whatever `targets` says.
    everyone: bool,
    /// Pumps owed a gap resync, deduplicated.
    targets: Vec<ResyncTarget>,
}

/// The debounced post-resize resync. Re-armed per resize; on fire, one
/// snapshot at the settled size. A reflow is owed to everyone, a gap only to
/// the pumps that asked; all share one deadline and one synthesis.
pub(in crate::actor) struct ResyncDebounce {
    /// The resync owed once the deadline lands, if any.
    owed: Option<Owed>,
}

impl ResyncDebounce {
    /// Nothing owed.
    pub(crate) const fn idle() -> Self {
        Self { owed: None }
    }

    /// Whether a resync is owed.
    pub(super) const fn is_owed(&self) -> bool {
        self.owed.is_some()
    }

    /// Whether the owed snapshot may fire (none is blocked by a bootstrap).
    pub(super) const fn may_fire(&self, bootstrap_pending: bool) -> bool {
        self.is_owed() && !bootstrap_pending
    }

    /// Owe a resync for `reason` and (re)start the debounce.
    ///
    /// An already-owed gap resync does not push the deadline out: N pumps
    /// retrying independently would otherwise keep resetting it and livelock.
    /// Resizes always re-arm (the last size wins). The audience is recorded
    /// first either way.
    pub(super) fn arm(
        &mut self,
        owed: OwedResync,
        deadline: std::pin::Pin<&mut tokio::time::Sleep>,
    ) {
        let coalesce_gap = self.is_owed() && owed.reason == ResyncReason::OutboundGap;
        self.include(owed);
        if coalesce_gap {
            return;
        }
        deadline.reset(tokio::time::Instant::now() + RESIZE_RESYNC_DEBOUNCE);
    }

    /// Fold `owed` into the audience without touching the deadline (PTY EOF:
    /// the snapshot is about to fire). A gap joining an owed resync keeps
    /// that resync's reason.
    pub(super) fn include(&mut self, owed: OwedResync) {
        let debt = self.owed.get_or_insert_with(|| Owed {
            reason: owed.reason,
            everyone: false,
            targets: Vec::new(),
        });
        match owed.target {
            None => debt.everyone = true,
            Some(target) if !debt.targets.contains(&target) => debt.targets.push(target),
            Some(_) => {}
        }
        if owed.reason != ResyncReason::OutboundGap {
            debt.reason = owed.reason;
        }
    }

    /// Clear the owed resync, returning its reason and audience.
    pub(super) fn take(&mut self) -> Option<(ResyncReason, ResyncAudience)> {
        self.owed.take().map(|debt| {
            let audience = if debt.everyone {
                ResyncAudience::Everyone
            } else {
                ResyncAudience::Only(debt.targets.into())
            };
            (debt.reason, audience)
        })
    }
}
