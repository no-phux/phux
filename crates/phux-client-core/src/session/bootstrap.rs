//! The Terminal lane's bootstrap staging: begin, chunk validation, limits,
//! and READY.

use super::lane::{BeginFrame, ChunkFrame, LaneCx, LaneRails, ReadyFrame};
use super::staging_rules::{
    StagingClock, chunk_admission, fence_replaced, reject_retired, retire_for_codec_failure,
};
use super::{
    EffectBuffer, EngineAdapter, EngineEffect, EngineEffectBuffer, HistoryCursor, KernelError,
    ReplicaKey, Staging, TerminalState, generation_of, mismatch_error, progressive_finish_missing,
};

impl<R> TerminalState<R> {
    pub(super) fn begin<E: EngineAdapter<Replica = R>>(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &BeginFrame,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        let terminal_id = cx.resource_id;
        let generation = frame.generation;
        reject_retired(&self.retired, terminal_id, generation)?;
        if self.published_matches(generation) || self.staging_matches(generation) {
            return Err(KernelError::DuplicateGeneration {
                terminal_id: terminal_id.clone(),
                stream_id: generation.stream_id,
                bootstrap_id: generation.bootstrap_id,
            });
        }

        cx.engine_effects.clear();
        let engine = match cx.adapter.start_replica(frame.profile, frame.geometry) {
            Ok(engine) => engine,
            Err(error) => {
                retire_for_codec_failure(
                    &mut self.retired,
                    terminal_id,
                    generation,
                    frame.base_seq,
                    effects,
                );
                return Err(KernelError::Engine(error));
            }
        };
        if let Some(old_staging) = self.staging.replace(Staging {
            key: ReplicaKey {
                terminal_id: terminal_id.clone(),
                stream_id: generation.stream_id,
                bootstrap_id: generation.bootstrap_id,
                profile: frame.profile,
            },
            geometry: frame.geometry,
            base_seq: frame.base_seq,
            next_chunk_seq: Some(0),
            engine_ready: false,
            protocol_ready: false,
            history_cursor: None,
            engine,
            pending_effects: Vec::new(),
            accepted_bytes: 0,
            accepted_chunks: 0,
            clock: StagingClock::start(cx.now_ms),
        }) {
            fence_replaced(
                &mut self.retired,
                generation_of(&old_staging.key),
                old_staging.base_seq,
            );
        }
        Ok(())
    }

    /// Bootstrap is replay into staging: publication supplies damage, and
    /// statuses and jobs wait for it.
    fn buffer_bootstrap_effects(staging: &mut Staging<R>, engine_effects: &mut EngineEffectBuffer) {
        for effect in engine_effects.drain() {
            match effect {
                EngineEffect::Damage(_) => {}
                effect @ (EngineEffect::Status(_) | EngineEffect::Job(_)) => {
                    staging.pending_effects.push(effect);
                }
            }
        }
    }

    /// Drop the staging generation after its engine failed and ask for a recut.
    fn retire_staging_codec_failure(
        &mut self,
        terminal_id: &phux_protocol::ResourceId,
        generation: super::GenerationId,
        last_valid_seq: u64,
        engine_effects: &mut EngineEffectBuffer,
        effects: &mut EffectBuffer,
    ) {
        engine_effects.clear();
        self.staging = None;
        retire_for_codec_failure(
            &mut self.retired,
            terminal_id,
            generation,
            last_valid_seq,
            effects,
        );
    }

    pub(super) fn chunk<E: EngineAdapter<Replica = R>>(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &ChunkFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        let terminal_id = cx.resource_id;
        let generation = frame.generation;
        reject_retired(&self.retired, terminal_id, generation)?;
        let staging = self
            .staging
            .as_ref()
            .filter(|staging| generation_of(&staging.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        if staging.engine_ready {
            return Err(KernelError::ChunkAfterEngineReady);
        }
        let admission = chunk_admission(
            staging.next_chunk_seq,
            frame.chunk_seq,
            staging.clock,
            staging.accepted_bytes.saturating_add(frame.payload.len()),
            cx.now_ms,
            frame.usage_allowed,
        );
        if let Err(error) = admission {
            // A sequence fault leaves the cut staged; only a limit retires it.
            if matches!(error, KernelError::BootstrapStagingLimitExceeded) {
                self.retire_for_limit(terminal_id, generation, effects);
            }
            return Err(error);
        }
        let staging = self
            .staging
            .as_mut()
            .filter(|staging| generation_of(&staging.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;

        cx.engine_effects.clear();
        let progress = match cx.adapter.apply_bootstrap_chunk(
            &mut staging.engine,
            frame.payload,
            cx.engine_effects,
        ) {
            Ok(progress) => progress,
            Err(error) => {
                let last_valid_seq = staging.base_seq;
                self.retire_staging_codec_failure(
                    terminal_id,
                    generation,
                    last_valid_seq,
                    cx.engine_effects,
                    effects,
                );
                return Err(KernelError::Engine(error));
            }
        };
        staging.next_chunk_seq = frame.chunk_seq.checked_add(1);
        staging.accepted_bytes = staging
            .accepted_bytes
            .checked_add(frame.payload.len())
            .ok_or(KernelError::BootstrapStagingLimitExceeded)?;
        staging.accepted_chunks = staging
            .accepted_chunks
            .checked_add(1)
            .ok_or(KernelError::BootstrapStagingLimitExceeded)?;
        staging.engine_ready |= progress.is_ready();
        staging.clock.progress(cx.now_ms);
        Self::buffer_bootstrap_effects(staging, cx.engine_effects);
        Ok(())
    }

    pub(super) fn ready<E: EngineAdapter<Replica = R>>(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &ReadyFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        let terminal_id = cx.resource_id;
        let generation = frame.generation;
        reject_retired(&self.retired, terminal_id, generation)?;
        let staging = self
            .staging
            .as_ref()
            .filter(|staging| generation_of(&staging.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        if staging.protocol_ready {
            return Err(KernelError::DuplicateProtocolReady);
        }
        if staging.clock.expired(staging.accepted_bytes, cx.now_ms) {
            self.retire_for_limit(terminal_id, generation, effects);
            return Err(KernelError::BootstrapStagingLimitExceeded);
        }
        let staging = self
            .staging
            .as_mut()
            .filter(|staging| generation_of(&staging.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        cx.engine_effects.clear();
        let progress = match cx
            .adapter
            .finish_bootstrap(&mut staging.engine, cx.engine_effects)
        {
            Ok(progress) => progress,
            Err(error) => {
                let last_valid_seq = staging.base_seq;
                self.retire_staging_codec_failure(
                    terminal_id,
                    generation,
                    last_valid_seq,
                    cx.engine_effects,
                    effects,
                );
                return Err(KernelError::Engine(error));
            }
        };
        let has_history_cursor = frame.history_cursor.is_some();
        if progressive_finish_missing(staging.key.profile, progress, has_history_cursor) {
            let last_valid_seq = staging.base_seq;
            self.retire_staging_codec_failure(
                terminal_id,
                generation,
                last_valid_seq,
                cx.engine_effects,
                effects,
            );
            return Err(KernelError::HistoryCompletionMismatch {
                progress,
                has_more: false,
            });
        }
        staging.history_cursor = frame.history_cursor.map(HistoryCursor::new);
        staging.protocol_ready = true;
        staging.engine_ready |= progress.is_ready();
        if !staging.engine_ready {
            let last_valid_seq = staging.base_seq;
            self.retire_staging_codec_failure(
                terminal_id,
                generation,
                last_valid_seq,
                cx.engine_effects,
                effects,
            );
            return Err(KernelError::EngineNotReady);
        }
        Self::buffer_bootstrap_effects(staging, cx.engine_effects);
        self.publish(cx, effects)
    }
}
