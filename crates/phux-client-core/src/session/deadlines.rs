//! Deadlines a synchronous kernel can only check when it is called: stalled
//! staging cuts and document recuts that never began.
//!
//! A host whose connection can fall silent calls
//! [`SessionKernel::expire_bootstrap_staging`] (or
//! [`SessionKernel::expire_bootstrap_staging_now`]) from a timer, and from
//! its first call owns the checks. Until then every update checks them
//! while one is pending, so a host without a timer still reclaims a stalled
//! cut (one left from an earlier connection, say) and learns of a recut
//! that never began, as long as frames keep arriving.

use super::lane::LaneRails;
use super::{
    EffectBuffer, EngineAdapter, KernelEffect, KernelInput, KernelStatus, ResourceId, SessionKernel,
};

impl<E: EngineAdapter> SessionKernel<E> {
    /// Retire every incomplete bootstrap whose staging clock expired, and
    /// report every document recut that has not begun in time. Each emits
    /// [`KernelStatus::ResyncRequired`] once; returns how many did.
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
        self.lanes
            .values()
            .filter_map(LaneRails::deadline_ms)
            .min()
            .map(|deadline_ms| deadline_ms.saturating_sub(now_ms))
    }

    /// The per-update check. A frame's own resource is left to its lane,
    /// which judges that frame with its precise error.
    pub(super) fn check_deadlines(&mut self, input: &KernelInput<'_>, effects: &mut EffectBuffer) {
        if !self.deadline_watch || self.host_ticks {
            return;
        }
        let now_ms = self.monotonic_now_ms();
        self.sweep_deadlines(now_ms, stream_resource(input), effects);
    }

    fn sweep_deadlines(
        &mut self,
        now_ms: u64,
        skip: Option<&ResourceId>,
        effects: &mut EffectBuffer,
    ) -> usize {
        let mut expired = Vec::new();
        let mut reported = 0;
        for (resource_id, lane) in self.lanes.iter_mut() {
            if skip == Some(resource_id) {
                continue;
            }
            if let Some(generation) = lane.expired_staging(now_ms) {
                expired.push((resource_id.clone(), generation));
            }
            if let Some((generation, reason)) = lane.take_overdue_recut(now_ms) {
                effects.push(KernelEffect::Status(KernelStatus::ResyncRequired {
                    terminal_id: resource_id.clone(),
                    stream_id: generation.stream_id,
                    bootstrap_id: generation.bootstrap_id,
                    reason,
                }));
                reported += 1;
            }
        }
        for (resource_id, generation) in &expired {
            self.retire_staging_limit(resource_id, *generation, effects);
        }
        self.deadline_watch = self.next_deadline_in_ms().is_some();
        reported + expired.len()
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
