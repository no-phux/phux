//! Deadlines a synchronous kernel can only check when it is called: stalled
//! staging cuts, document recuts that never began, and streams whose frames
//! keep naming a generation that is not current.
//!
//! A host whose connection can fall silent calls
//! [`SessionKernel::expire_bootstrap_staging`] (or
//! [`SessionKernel::expire_bootstrap_staging_now`]) from a timer, and from
//! its first call owns the checks. Until then an update checks them, as
//! long as frames keep arriving: it reads the clock only while a deadline
//! is pending and scans the lanes only once the earliest one is due.

use super::lane::{LaneRails, ResourceLane};
use super::{
    EffectBuffer, EngineAdapter, GenerationId, KernelEffect, KernelError, KernelInput,
    KernelStatus, ResourceId, SessionKernel, TombstoneReason,
};

/// Consecutive frames off the current generations one resource may send
/// before the kernel stops treating them as late and asks for recovery: a
/// mis-tagging peer must not freeze a view silently.
pub(super) const MAX_BENIGN_DROPS: u32 = 64;

impl<E: EngineAdapter> SessionKernel<E> {
    /// Retire every incomplete bootstrap whose staging clock expired, and
    /// report every stalled document. A Terminal or `AgentSession` cut emits
    /// [`KernelStatus::ResyncRequired`]; a document emits
    /// [`KernelStatus::DocumentStalled`] or [`KernelStatus::DocumentLost`].
    /// Returns how many were reported.
    ///
    /// Drivers should call this from their existing connection timer using
    /// the same monotonic millisecond source passed to [`Self::update_at`].
    pub fn expire_bootstrap_staging(&mut self, now_ms: u64, effects: &mut EffectBuffer) -> usize {
        self.inject_now(now_ms);
        self.host_ticks = true;
        effects.clear();
        self.sweep_deadlines(self.monotonic_now_ms(), None, effects)
    }

    /// [`Self::expire_bootstrap_staging`] on the kernel's own clock, for a
    /// native host that drives [`Self::update`] without timestamps.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn expire_bootstrap_staging_now(&mut self, effects: &mut EffectBuffer) -> usize {
        self.host_ticks = true;
        effects.clear();
        self.sweep_deadlines(self.monotonic_now_ms(), None, effects)
    }

    /// Milliseconds on the kernel's clock until the next staging cut or
    /// awaited recut is overdue (`0` once one is); `None` when nothing is
    /// pending. A host arms its timer for this.
    #[must_use]
    pub fn next_deadline_in_ms(&self) -> Option<u64> {
        let now_ms = self.monotonic_now_ms();
        self.earliest_deadline_ms()
            .map(|deadline_ms| deadline_ms.saturating_sub(now_ms))
    }

    fn earliest_deadline_ms(&self) -> Option<u64> {
        self.lanes.values().filter_map(LaneRails::deadline_ms).min()
    }

    /// A lane's deadline moved earlier (a cut opened, a recut is awaited).
    pub(super) fn watch_deadline(&mut self, deadline_ms: Option<u64>) {
        self.next_deadline_ms = match (self.next_deadline_ms, deadline_ms) {
            (Some(current), Some(new)) => Some(current.min(new)),
            (current, new) => current.or(new),
        };
    }

    /// The per-update check: one clock read while a deadline is pending, a
    /// lane scan only once it is due. A frame's own resource is left to its
    /// lane, which judges that frame with its precise error.
    pub(super) fn check_deadlines(&mut self, input: &KernelInput<'_>, effects: &mut EffectBuffer) {
        if self.host_ticks {
            return;
        }
        let Some(due_ms) = self.next_deadline_ms else {
            return;
        };
        let now_ms = self.monotonic_now_ms();
        if now_ms < due_ms {
            return;
        }
        self.sweep_deadlines(now_ms, stream_resource(input), effects);
    }

    fn sweep_deadlines(
        &mut self,
        now_ms: u64,
        skip: Option<&ResourceId>,
        effects: &mut EffectBuffer,
    ) -> usize {
        #[cfg(test)]
        {
            self.deadline_sweeps += 1;
        }
        let mut expired = Vec::new();
        let mut reported = 0;
        for (resource_id, lane) in self.lanes.iter_mut() {
            if skip == Some(resource_id) {
                continue;
            }
            if let Some(status) = lane.take_stall(resource_id, now_ms) {
                effects.push(KernelEffect::Status(status));
                reported += 1;
            } else if let Some(generation) = lane.expired_staging(now_ms) {
                expired.push((resource_id.clone(), generation));
            }
        }
        for (resource_id, generation) in &expired {
            self.retire_staging_limit(resource_id, *generation, effects);
        }
        self.next_deadline_ms = self.earliest_deadline_ms();
        reported + expired.len()
    }

    /// Count a frame dropped off the current generations; a run of them
    /// past [`MAX_BENIGN_DROPS`] asks for recovery. Any accepted frame ends
    /// the run.
    pub(super) fn tally_benign<X>(
        &mut self,
        resource_id: &ResourceId,
        generation: GenerationId,
        result: &Result<(), KernelError<X>>,
        effects: &mut EffectBuffer,
    ) {
        match result {
            Ok(()) if !self.benign_drops.is_empty() => {
                self.benign_drops.remove(resource_id);
            }
            Err(error) if error.is_benign_generation() => {
                let drops = self.benign_drops.entry(resource_id.clone()).or_insert(0);
                *drops += 1;
                if *drops >= MAX_BENIGN_DROPS {
                    self.benign_drops.remove(resource_id);
                    self.escalate_benign(resource_id, generation, effects);
                }
            }
            _ => {}
        }
    }

    /// A document is attached again on its own; any other stream resyncs.
    fn escalate_benign(
        &mut self,
        resource_id: &ResourceId,
        generation: GenerationId,
        effects: &mut EffectBuffer,
    ) {
        let reason = TombstoneReason::Other;
        let now_ms = self.monotonic_now_ms();
        let status = if let Some(ResourceLane::Document(stream)) = self.lanes.get_mut(resource_id) {
            let status = stream.stall(resource_id, reason, now_ms);
            let deadline_ms = stream.deadline_ms();
            self.watch_deadline(deadline_ms);
            status
        } else {
            KernelStatus::ResyncRequired {
                terminal_id: resource_id.clone(),
                stream_id: generation.stream_id,
                bootstrap_id: generation.bootstrap_id,
                reason,
            }
        };
        effects.push(KernelEffect::Status(status));
    }
}

/// The resource a stream frame names.
const fn stream_resource<'a>(input: &KernelInput<'a>) -> Option<&'a ResourceId> {
    match input {
        KernelInput::BootstrapBegin { terminal_id, .. }
        | KernelInput::BootstrapChunk { terminal_id, .. }
        | KernelInput::BootstrapReady { terminal_id, .. }
        | KernelInput::ResourceOutput { terminal_id, .. }
        | KernelInput::Tombstone { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}
