//! Stalled document streams, recovered one resource at a time.
//!
//! A server that fences a lapped viewer recuts it at once. A relay that
//! drops a viewer it cannot deliver to fences it and recuts nothing, so a
//! fence of the live generation (or of one this viewer never saw, while
//! nothing is live) arms a recut watch, and any `BOOTSTRAP_BEGIN` clears it.
//! A cut that stops arriving, or a watch that runs out, is a stall: the
//! host attaches the resource again and the watch waits for that recut,
//! each time twice as long up to four idle windows. After
//! [`MAX_DOCUMENT_REATTACHES`] stalls with no document delivered between
//! them, the document is reported lost.

use super::super::{
    GenerationId, KernelStatus, MAX_BOOTSTRAP_STAGING_IDLE_MS, ResourceId, TombstoneReason,
    TombstoneRecord,
};
use super::DocumentStream;

/// Re-attaches asked for before a stalled document is reported lost.
pub(in crate::session) const MAX_DOCUMENT_REATTACHES: u32 = 3;

/// A recut is awaited since `since_ms`; `reason` says why the stream was
/// fenced.
#[derive(Clone, Copy)]
pub(super) struct RecutWatch {
    reason: TombstoneReason,
    since_ms: u64,
}

impl DocumentStream {
    /// Record an external tombstone; staging for that generation is dropped.
    /// Fencing the live generation (the cut in flight, or the published one
    /// when none is), or any generation while nothing is live, arms the
    /// recut watch.
    pub(super) fn note_external_tombstone(
        &mut self,
        generation: GenerationId,
        record: TombstoneRecord,
        now_ms: u64,
    ) {
        let live = match (&self.staging, self.published) {
            (Some(cut), _) => cut.generation == generation,
            (None, Some((published, _))) => published == generation,
            (None, None) => true,
        };
        if live {
            self.await_recut(record.reason, now_ms);
        }
        self.retired.entry(generation).or_insert(record);
        if self.staging_matches(generation) {
            self.staging = None;
        }
    }

    /// Wait for a recut from `now_ms`, unless a wait is already running.
    pub(super) fn await_recut(&mut self, reason: TombstoneReason, now_ms: u64) {
        self.recut_watch.get_or_insert(RecutWatch {
            reason,
            since_ms: now_ms,
        });
    }

    /// Whether neither a cut nor a published document is live.
    pub(super) const fn is_idle(&self) -> bool {
        self.staging.is_none() && self.published.is_none()
    }

    /// The first millisecond the cut in flight or the awaited recut is
    /// overdue.
    pub(super) fn deadline_ms(&self) -> Option<u64> {
        let staging = self
            .staging
            .as_ref()
            .map(|cut| cut.clock.deadline_ms(cut.buffer.len()));
        staging.into_iter().chain(self.recut_deadline_ms()).min()
    }

    /// The idle window doubles with each stall, up to four windows.
    fn recut_deadline_ms(&self) -> Option<u64> {
        let watch = self.recut_watch?;
        let window = MAX_BOOTSTRAP_STAGING_IDLE_MS << self.stalls.min(2);
        Some(watch.since_ms.saturating_add(window).saturating_add(1))
    }

    /// The stall due at `now_ms`, if any: an expired cut is fenced, and
    /// either way the stall is counted and reported once.
    pub(super) fn take_stall(
        &mut self,
        resource_id: &ResourceId,
        now_ms: u64,
    ) -> Option<KernelStatus> {
        let expired = self
            .staging
            .take_if(|cut| cut.clock.expired(cut.buffer.len(), now_ms));
        let reason = if let Some(cut) = expired {
            self.retired.insert(
                cut.generation,
                TombstoneRecord {
                    reason: TombstoneReason::Other,
                    last_valid_seq: cut.base_seq,
                },
            );
            TombstoneReason::Other
        } else {
            let due = self.recut_deadline_ms().is_some_and(|due| now_ms >= due);
            self.recut_watch.filter(|_| due)?.reason
        };
        Some(self.stall(resource_id, reason, now_ms))
    }

    /// Count one stall: ask for a re-attach and wait for its recut, or,
    /// past the cap, give the document up.
    pub(in crate::session) fn stall(
        &mut self,
        resource_id: &ResourceId,
        reason: TombstoneReason,
        now_ms: u64,
    ) -> KernelStatus {
        self.stalls = self.stalls.saturating_add(1);
        if self.stalls > MAX_DOCUMENT_REATTACHES {
            self.recut_watch = None;
            return KernelStatus::DocumentLost {
                terminal_id: resource_id.clone(),
                reason,
            };
        }
        self.recut_watch = Some(RecutWatch {
            reason,
            since_ms: now_ms,
        });
        KernelStatus::DocumentStalled {
            terminal_id: resource_id.clone(),
            reason,
            attempt: self.stalls,
        }
    }
}
