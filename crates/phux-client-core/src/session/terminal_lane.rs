//! The Terminal lane: one engine replica per generation.
//!
//! Staging, READY and publication live in [`bootstrap`](super::bootstrap)
//! and live output in [`output`](super::output); this module is the lane's
//! state and the rails it shares with every other kind.

use std::collections::HashMap;

use phux_protocol::{BootstrapProfile, ResourceId};

use super::lane::{
    AttachSignal, BeginFrame, ChunkFrame, KindLane, LaneCx, LaneRails, OutputFrame, ReadyFrame,
};
use super::staging_rules::{retire_for_codec_failure, staging_lifetime_exceeded};
use super::{
    EffectBuffer, EngineAdapter, GenerationId, KernelError, Replica, Staging, TombstoneRecord,
    generation_of, profile_matches, retained_engine_effect_bytes,
};

pub(super) struct TerminalState<R> {
    pub(super) published: Option<Replica<R>>,
    pub(super) staging: Option<Staging<R>>,
    pub(super) retired: HashMap<GenerationId, TombstoneRecord>,
}

impl<R> Default for TerminalState<R> {
    fn default() -> Self {
        Self {
            published: None,
            staging: None,
            retired: HashMap::new(),
        }
    }
}

/// A Terminal replica is bound to the connection profile and needs a
/// non-empty canonical geometry; both are checked before any lane state.
pub(super) const fn admit_begin<X>(
    selected: BootstrapProfile,
    frame: &BeginFrame,
) -> Result<(), KernelError<X>> {
    if !profile_matches(selected, frame.profile) {
        return Err(KernelError::ProfileMismatch {
            selected,
            incoming: frame.profile,
        });
    }
    if frame.geometry.cols == 0 || frame.geometry.rows == 0 {
        return Err(KernelError::InvalidGeometry {
            cols: frame.geometry.cols,
            rows: frame.geometry.rows,
        });
    }
    Ok(())
}

impl<R> TerminalState<R> {
    pub(super) fn published_matches(&self, generation: GenerationId) -> bool {
        self.published
            .as_ref()
            .is_some_and(|replica| generation_of(&replica.key) == generation)
    }

    pub(super) fn staging_matches(&self, generation: GenerationId) -> bool {
        self.staging
            .as_ref()
            .is_some_and(|staging| generation_of(&staging.key) == generation)
    }
}

impl<R> LaneRails for TerminalState<R> {
    fn on_tombstone(&mut self, generation: GenerationId, record: TombstoneRecord) -> AttachSignal {
        self.retired.entry(generation).or_insert(record);
        if self.staging_matches(generation) {
            self.staging = None;
        }
        if self.published_matches(generation) {
            AttachSignal::Unresolved {
                pending_removal: false,
            }
        } else {
            AttachSignal::Unchanged
        }
    }

    fn staged_accounting(&self) -> Option<(usize, usize)> {
        self.staging
            .as_ref()
            .map(|staging| (staging.accepted_bytes, staging.accepted_chunks))
    }

    fn expired_staging(&self, now_ms: u64) -> Option<GenerationId> {
        let staging = self.staging.as_ref()?;
        staging_lifetime_exceeded(staging.started_at_ms, now_ms)
            .then(|| generation_of(&staging.key))
    }

    fn retire_for_limit(
        &mut self,
        resource_id: &ResourceId,
        generation: GenerationId,
        effects: &mut EffectBuffer,
    ) {
        let last_valid_seq = self.staging.take().map_or(0, |staging| staging.base_seq);
        retire_for_codec_failure(
            &mut self.retired,
            resource_id,
            generation,
            last_valid_seq,
            effects,
        );
    }
}

impl<E: EngineAdapter> KindLane<E> for TerminalState<E::Replica> {
    fn on_begin(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &BeginFrame,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.begin(cx, frame, effects)
    }

    fn on_chunk(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &ChunkFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.chunk(cx, frame, effects)
    }

    fn on_ready(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &ReadyFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ready(cx, frame, effects)
    }

    fn on_output(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &OutputFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.output(cx, frame, effects)
    }

    fn retained_staging_bytes(&self, adapter: &E) -> usize {
        self.staging.as_ref().map_or(0, |staging| {
            staging
                .accepted_bytes
                .saturating_add(adapter.bootstrap_staging_bytes(&staging.engine))
                .saturating_add(retained_engine_effect_bytes(
                    staging.pending_effects.capacity(),
                    &staging.pending_effects,
                ))
        })
    }
}
