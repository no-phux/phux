//! Bootstrap staging: begin, chunk validation, limits, and READY.

use super::{
    AgentStream, BootstrapId, BootstrapStreamProfile, CanonicalGeometry, EffectBuffer,
    EngineAdapter, GenerationId, HistoryCursor, KernelEffect, KernelError, KernelStatus,
    MAX_BOOTSTRAP_STAGING_BYTES, MAX_BOOTSTRAP_STAGING_CHUNKS, ReplicaKey, ResourceId,
    SessionKernel, Staging, StreamId, TombstoneReason, TombstoneRecord, generation_of,
    mismatch_error, profile_matches, progressive_finish_missing, retained_engine_effect_bytes,
    retired_error, staging_expired,
};

impl<E: EngineAdapter> SessionKernel<E> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn bootstrap_begin(
        &mut self,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        profile: BootstrapStreamProfile,
        geometry: CanonicalGeometry,
        base_seq: u64,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        let started_at_ms = self.monotonic_now_ms();
        if self.closed.contains(terminal_id) {
            return Err(KernelError::ClosedTerminal(terminal_id.clone()));
        }
        if !profile_matches(self.selected_profile, profile) {
            return Err(KernelError::ProfileMismatch {
                selected: self.selected_profile,
                incoming: profile,
            });
        }
        if geometry.cols == 0 || geometry.rows == 0 {
            return Err(KernelError::InvalidGeometry {
                cols: geometry.cols,
                rows: geometry.rows,
            });
        }

        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let state = self.terminals.entry(terminal_id.clone()).or_default();
        if state.retired.contains_key(&generation) {
            return Err(KernelError::RetiredGeneration {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
            });
        }
        if state
            .published
            .as_ref()
            .is_some_and(|replica| generation_of(&replica.key) == generation)
            || state
                .staging
                .as_ref()
                .is_some_and(|staging| generation_of(&staging.key) == generation)
        {
            return Err(KernelError::DuplicateGeneration {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
            });
        }

        self.engine_effects.clear();
        let engine = match self.adapter.start_replica(profile, geometry) {
            Ok(engine) => engine,
            Err(error) => {
                state.retired.insert(
                    generation,
                    TombstoneRecord {
                        reason: TombstoneReason::CodecFailure,
                        last_valid_seq: base_seq,
                    },
                );
                effects.push(KernelEffect::Status(KernelStatus::ResyncRequired {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    reason: TombstoneReason::CodecFailure,
                }));
                return Err(KernelError::Engine(error));
            }
        };
        if let Some(old_staging) = state.staging.replace(Staging {
            key: ReplicaKey {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile,
            },
            geometry,
            base_seq,
            next_chunk_seq: Some(0),
            engine_ready: false,
            protocol_ready: false,
            history_cursor: None,
            engine,
            pending_effects: Vec::new(),
            accepted_bytes: 0,
            accepted_chunks: 0,
            started_at_ms,
        }) {
            state.retired.insert(
                generation_of(&old_staging.key),
                TombstoneRecord {
                    reason: TombstoneReason::ExplicitReattach,
                    last_valid_seq: old_staging.base_seq,
                },
            );
        }
        Ok(())
    }

    pub(super) fn next_bootstrap_staging_usage(
        &self,
        payload_len: usize,
    ) -> Option<(usize, usize)> {
        let mut bytes = payload_len;
        let mut chunks = 1_usize;
        for (accepted_bytes, accepted_chunks) in self
            .agents
            .values()
            .filter_map(AgentStream::staged_accounting)
        {
            bytes = bytes.checked_add(accepted_bytes)?;
            chunks = chunks.checked_add(accepted_chunks)?;
        }
        for staging in self
            .terminals
            .values()
            .filter_map(|state| state.staging.as_ref())
        {
            bytes = bytes.checked_add(staging.accepted_bytes)?;
            chunks = chunks.checked_add(staging.accepted_chunks)?;
        }
        (bytes <= MAX_BOOTSTRAP_STAGING_BYTES && chunks <= MAX_BOOTSTRAP_STAGING_CHUNKS)
            .then_some((bytes, chunks))
    }

    fn bootstrap_retained_bytes(&self) -> Option<usize> {
        self.terminals
            .values()
            .filter_map(|state| state.staging.as_ref())
            .try_fold(0_usize, |total, staging| {
                total
                    .checked_add(staging.accepted_bytes)?
                    .checked_add(self.adapter.bootstrap_staging_bytes(&staging.engine))?
                    .checked_add(retained_engine_effect_bytes(
                        staging.pending_effects.capacity(),
                        &staging.pending_effects,
                    ))
            })
    }

    fn validate_bootstrap_chunk(
        &self,
        terminal_id: &ResourceId,
        generation: GenerationId,
        chunk_seq: u32,
    ) -> Result<(), KernelError<E::Error>> {
        let state = self
            .terminals
            .get(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        if state.retired.contains_key(&generation) {
            return Err(retired_error(terminal_id, generation));
        }
        let staging = state
            .staging
            .as_ref()
            .filter(|staging| generation_of(&staging.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        if staging.engine_ready {
            return Err(KernelError::ChunkAfterEngineReady);
        }
        let Some(expected) = staging.next_chunk_seq else {
            return Err(KernelError::ChunkSequenceExhausted);
        };
        match chunk_seq.cmp(&expected) {
            std::cmp::Ordering::Less => Err(KernelError::DuplicateChunk {
                expected,
                actual: chunk_seq,
            }),
            std::cmp::Ordering::Greater => Err(KernelError::ChunkGap {
                expected,
                actual: chunk_seq,
            }),
            std::cmp::Ordering::Equal => Ok(()),
        }
    }

    fn bootstrap_staging_limit_exceeded(
        &self,
        terminal_id: &ResourceId,
        generation: GenerationId,
        payload_len: usize,
    ) -> bool {
        let expired = self
            .terminals
            .get(terminal_id)
            .and_then(|state| state.staging.as_ref())
            .filter(|staging| generation_of(&staging.key) == generation)
            .is_some_and(|staging| staging_expired(staging, self.monotonic_now_ms()));
        expired || self.next_bootstrap_staging_usage(payload_len).is_none()
    }

    fn validate_bootstrap_ready(
        &self,
        terminal_id: &ResourceId,
        generation: GenerationId,
    ) -> Result<bool, KernelError<E::Error>> {
        let state = self
            .terminals
            .get(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        if state.retired.contains_key(&generation) {
            return Err(retired_error(terminal_id, generation));
        }
        let staging = state
            .staging
            .as_ref()
            .filter(|staging| generation_of(&staging.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        if staging.protocol_ready {
            return Err(KernelError::DuplicateProtocolReady);
        }
        Ok(staging_expired(staging, self.monotonic_now_ms()))
    }

    pub(super) fn retire_staging_limit(
        &mut self,
        terminal_id: &ResourceId,
        generation: GenerationId,
        effects: &mut EffectBuffer,
    ) {
        if let Some(stream) = self.agents.get_mut(terminal_id) {
            stream.retire_for_limit(terminal_id, generation, effects);
            return;
        }
        let Some(state) = self.terminals.get_mut(terminal_id) else {
            return;
        };
        let last_valid_seq = state.staging.take().map_or(0, |staging| staging.base_seq);
        state.retired.insert(
            generation,
            TombstoneRecord {
                reason: TombstoneReason::CodecFailure,
                last_valid_seq,
            },
        );
        effects.push(KernelEffect::Status(KernelStatus::ResyncRequired {
            terminal_id: terminal_id.clone(),
            stream_id: generation.stream_id,
            bootstrap_id: generation.bootstrap_id,
            reason: TombstoneReason::CodecFailure,
        }));
    }

    pub(super) fn bootstrap_chunk(
        &mut self,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        chunk_seq: u32,
        payload: &[u8],
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        self.validate_bootstrap_chunk(terminal_id, generation, chunk_seq)?;
        if self.bootstrap_staging_limit_exceeded(terminal_id, generation, payload.len()) {
            self.retire_staging_limit(terminal_id, generation, effects);
            return Err(KernelError::BootstrapStagingLimitExceeded);
        }
        let state = self
            .terminals
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        let staging = state
            .staging
            .as_mut()
            .filter(|staging| generation_of(&staging.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;

        self.engine_effects.clear();
        let progress = match self.adapter.apply_bootstrap_chunk(
            &mut staging.engine,
            payload,
            &mut self.engine_effects,
        ) {
            Ok(progress) => progress,
            Err(error) => {
                let last_valid_seq = staging.base_seq;
                self.engine_effects.clear();
                state.staging = None;
                state.retired.insert(
                    generation,
                    TombstoneRecord {
                        reason: TombstoneReason::CodecFailure,
                        last_valid_seq,
                    },
                );
                effects.push(KernelEffect::Status(KernelStatus::ResyncRequired {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    reason: TombstoneReason::CodecFailure,
                }));
                return Err(KernelError::Engine(error));
            }
        };
        staging.next_chunk_seq = chunk_seq.checked_add(1);
        staging.accepted_bytes = staging
            .accepted_bytes
            .checked_add(payload.len())
            .ok_or(KernelError::BootstrapStagingLimitExceeded)?;
        staging.accepted_chunks = staging
            .accepted_chunks
            .checked_add(1)
            .ok_or(KernelError::BootstrapStagingLimitExceeded)?;
        staging.engine_ready |= progress.is_ready();
        self.buffer_bootstrap_effects(terminal_id, generation);
        if self
            .bootstrap_retained_bytes()
            .is_none_or(|bytes| bytes > MAX_BOOTSTRAP_STAGING_BYTES)
        {
            self.retire_staging_limit(terminal_id, generation, effects);
            return Err(KernelError::BootstrapStagingLimitExceeded);
        }
        Ok(())
    }

    pub(super) fn bootstrap_ready(
        &mut self,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        history_cursor: Option<&[u8]>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        if self.validate_bootstrap_ready(terminal_id, generation)? {
            self.retire_staging_limit(terminal_id, generation, effects);
            return Err(KernelError::BootstrapStagingLimitExceeded);
        }
        let state = self
            .terminals
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        let staging = state
            .staging
            .as_mut()
            .filter(|staging| generation_of(&staging.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        self.engine_effects.clear();
        let progress = match self
            .adapter
            .finish_bootstrap(&mut staging.engine, &mut self.engine_effects)
        {
            Ok(progress) => progress,
            Err(error) => {
                let last_valid_seq = staging.base_seq;
                self.engine_effects.clear();
                state.staging = None;
                state.retired.insert(
                    generation,
                    TombstoneRecord {
                        reason: TombstoneReason::CodecFailure,
                        last_valid_seq,
                    },
                );
                effects.push(KernelEffect::Status(KernelStatus::ResyncRequired {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    reason: TombstoneReason::CodecFailure,
                }));
                return Err(KernelError::Engine(error));
            }
        };
        let requires_finish =
            progressive_finish_missing(staging.key.profile, progress, history_cursor.is_some());
        if requires_finish {
            let last_valid_seq = staging.base_seq;
            self.engine_effects.clear();
            state.staging = None;
            state.retired.insert(
                generation,
                TombstoneRecord {
                    reason: TombstoneReason::CodecFailure,
                    last_valid_seq,
                },
            );
            effects.push(KernelEffect::Status(KernelStatus::ResyncRequired {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                reason: TombstoneReason::CodecFailure,
            }));
            return Err(KernelError::HistoryCompletionMismatch {
                progress,
                has_more: false,
            });
        }
        staging.history_cursor = history_cursor.map(HistoryCursor::new);
        staging.protocol_ready = true;
        staging.engine_ready |= progress.is_ready();
        let engine_ready = staging.engine_ready;
        if !engine_ready {
            let last_valid_seq = staging.base_seq;
            state.staging = None;
            state.retired.insert(
                generation,
                TombstoneRecord {
                    reason: TombstoneReason::CodecFailure,
                    last_valid_seq,
                },
            );
        }
        if engine_ready {
            self.buffer_bootstrap_effects(terminal_id, generation);
        } else {
            self.engine_effects.clear();
        }
        if !engine_ready {
            effects.push(KernelEffect::Status(KernelStatus::ResyncRequired {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                reason: TombstoneReason::CodecFailure,
            }));
            return Err(KernelError::EngineNotReady);
        }

        self.publish(terminal_id, effects)
    }
}
