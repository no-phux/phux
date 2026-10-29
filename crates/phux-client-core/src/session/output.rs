//! Publication of staged generations and live terminal output.

use super::{
    BootstrapId, BootstrapStreamProfile, DocumentAnchorId, EffectBuffer, EngineAdapter,
    GenerationId, HashMap, HistoryCache, HistoryUnavailableReason, KernelDamage, KernelDamageKind,
    KernelEffect, KernelError, KernelSend, KernelStatus, Replica, ReplicaKey, ResourceId,
    SessionKernel, StreamId, TerminalState, TombstoneReason, TombstoneRecord, ViewportAnchor,
    generation_of, mismatch_error, retired_error,
};

impl<E: EngineAdapter> SessionKernel<E> {
    pub(super) fn publish(
        &mut self,
        terminal_id: &ResourceId,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        let history_config = self.history_config;
        let (replica_key, pending_effects, initial_history_cursor, history_status) = {
            let state = self
                .terminals
                .get_mut(terminal_id)
                .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
            {
                let staging = state
                    .staging
                    .as_mut()
                    .ok_or_else(|| KernelError::MissingStaging(terminal_id.clone()))?;
                debug_assert!(staging.engine_ready && staging.protocol_ready);
                self.adapter
                    .configure_history_budget(
                        &mut staging.engine,
                        history_config.max_bytes,
                        history_config.max_materialized_rows,
                    )
                    .map_err(KernelError::Engine)?;
            }
            let mut staging = state
                .staging
                .take()
                .ok_or_else(|| KernelError::MissingStaging(terminal_id.clone()))?;
            let replica_key = staging.key.clone();
            let pending_effects = std::mem::take(&mut staging.pending_effects);
            let mut history = HistoryCache::new(
                history_config,
                staging.history_cursor.take(),
                staging.geometry.cols,
            );
            let initial_history_cursor = history.begin_fetch();
            let replacement = Replica {
                key: staging.key,
                geometry: staging.geometry,
                last_seq: staging.base_seq,
                next_seq: staging.base_seq.checked_add(1),
                engine: staging.engine,
                history,
            };
            let history_status = replacement.history.status();
            if let Some(old) = state.published.replace(replacement) {
                state
                    .retired
                    .entry(generation_of(&old.key))
                    .or_insert(TombstoneRecord {
                        reason: TombstoneReason::ExplicitReattach,
                        last_valid_seq: old.last_seq,
                    });
            }
            (
                replica_key,
                pending_effects,
                initial_history_cursor,
                history_status,
            )
        };

        self.mark_attach_resolved(terminal_id);
        let damage_allowed = !self.attach_blocks(terminal_id);
        for effect in pending_effects {
            Self::translate_engine_effect(&replica_key, effect, damage_allowed, effects);
        }
        if let Some(cursor) = initial_history_cursor {
            effects.push(KernelEffect::Send(KernelSend::HistoryRequest {
                key: replica_key.clone(),
                cursor: cursor.as_bytes().to_vec(),
                max_bytes: history_config.request_max_bytes,
                max_rows: history_config.request_max_rows,
            }));
            effects.push(KernelEffect::Status(KernelStatus::History {
                key: replica_key.clone(),
                status: history_status,
            }));
        }
        if damage_allowed {
            effects.push(KernelEffect::Damage(KernelDamage {
                terminal_id: terminal_id.clone(),
                kind: KernelDamageKind::Full,
            }));
        }
        Ok(())
    }

    /// Resolve the published replica for `generation`, or the error that
    /// says why it cannot accept the frame: the generation is retired,
    /// the terminal is unknown, or another generation is published.
    pub(super) fn published_replica<'a>(
        terminals: &'a mut HashMap<ResourceId, TerminalState<E::Replica>>,
        terminal_id: &ResourceId,
        generation: GenerationId,
    ) -> Result<&'a mut Replica<E::Replica>, KernelError<E::Error>> {
        let state = terminals
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        if state.retired.contains_key(&generation) {
            return Err(retired_error(terminal_id, generation));
        }
        state
            .published
            .as_mut()
            .filter(|replica| generation_of(&replica.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))
    }

    /// Live output is exactly sequenced: the replica accepts `expected`
    /// and nothing else.
    const fn expect_next_seq(
        replica: &Replica<E::Replica>,
        seq: u64,
    ) -> Result<(), KernelError<E::Error>> {
        let Some(expected) = replica.next_seq else {
            return Err(KernelError::SequenceExhausted);
        };
        if seq < expected {
            return Err(KernelError::DuplicateSequence {
                expected,
                actual: seq,
            });
        }
        if seq > expected {
            return Err(KernelError::SequenceGap {
                expected,
                actual: seq,
            });
        }
        Ok(())
    }

    /// Report why progressive history became unavailable, together with
    /// the status the cache reports afterwards.
    fn report_history_unavailable(
        replica: &Replica<E::Replica>,
        reason: HistoryUnavailableReason,
        effects: &mut EffectBuffer,
    ) {
        effects.push(KernelEffect::Status(KernelStatus::HistoryUnavailable {
            key: replica.key.clone(),
            reason,
        }));
        effects.push(KernelEffect::Status(KernelStatus::History {
            key: replica.key.clone(),
            status: replica.history.status(),
        }));
    }

    /// Invalidate only progressive history: tombstone the cache, release
    /// the engine's tracked document state, and report the reason. The
    /// published live replica remains authoritative and usable.
    pub(super) fn tombstone_history(
        adapter: &mut E,
        replica: &mut Replica<E::Replica>,
        reason: HistoryUnavailableReason,
        effects: &mut EffectBuffer,
    ) {
        replica.history.tombstone();
        adapter.clear_document_state(&mut replica.engine);
        Self::report_history_unavailable(replica, reason, effects);
    }

    /// Re-measure a pinned viewport anchor after live output landed:
    /// note how far the tail moved, or retire history when the anchor
    /// was pruned or can no longer be resolved.
    fn reconcile_pinned_anchor(
        adapter: &mut E,
        replica: &mut Replica<E::Replica>,
        anchor: DocumentAnchorId,
        distance_before: Option<u64>,
        effects: &mut EffectBuffer,
    ) {
        match (
            distance_before,
            adapter.history_anchor_tail_distance(&replica.engine, anchor),
        ) {
            (Some(before), Ok(Some(after))) => {
                replica
                    .history
                    .note_live_output(after.saturating_sub(before));
            }
            (None, Ok(Some(_))) | (_, Ok(None)) => {
                replica.history.mark_pruned();
                adapter.clear_document_state(&mut replica.engine);
                Self::report_history_unavailable(
                    replica,
                    HistoryUnavailableReason::Pruned,
                    effects,
                );
            }
            (_, Err(_)) => {
                Self::tombstone_history(
                    adapter,
                    replica,
                    HistoryUnavailableReason::CodecFailure,
                    effects,
                );
            }
        }
    }

    /// Retire the generation whose engine could not decode live output:
    /// drop the published replica, tombstone the generation, and ask the
    /// frontend for a resync.
    fn retire_after_codec_failure(
        &mut self,
        terminal_id: &ResourceId,
        generation: GenerationId,
        last_valid_seq: u64,
        effects: &mut EffectBuffer,
    ) {
        self.engine_effects.clear();
        if let Some(state) = self.terminals.get_mut(terminal_id) {
            state.published = None;
            state.retired.insert(
                generation,
                TombstoneRecord {
                    reason: TombstoneReason::CodecFailure,
                    last_valid_seq,
                },
            );
        }
        let damage_blocked = self.attach_blocks(terminal_id);
        self.mark_attach_unresolved(terminal_id, damage_blocked);
        effects.push(KernelEffect::Status(KernelStatus::ResyncRequired {
            terminal_id: terminal_id.clone(),
            stream_id: generation.stream_id,
            bootstrap_id: generation.bootstrap_id,
            reason: TombstoneReason::CodecFailure,
        }));
        if !damage_blocked {
            effects.push(KernelEffect::Damage(KernelDamage {
                terminal_id: terminal_id.clone(),
                kind: KernelDamageKind::Removed,
            }));
        }
    }

    /// Translate everything the engine just produced into kernel effects,
    /// keeping the buffer allocation for the next apply.
    pub(super) fn drain_engine_effects(
        &mut self,
        replica_key: &ReplicaKey,
        damage_allowed: bool,
        effects: &mut EffectBuffer,
    ) {
        let mut captured = std::mem::take(&mut self.engine_effects);
        for effect in captured.drain() {
            Self::translate_engine_effect(replica_key, effect, damage_allowed, effects);
        }
        self.engine_effects = captured;
    }

    pub(super) fn terminal_output(
        &mut self,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        seq: u64,
        payload: &[u8],
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let replica = Self::published_replica(&mut self.terminals, terminal_id, generation)?;
        Self::expect_next_seq(replica, seq)?;

        let pinned_anchor = match replica.history.viewport_anchor() {
            ViewportAnchor::Pinned(anchor) => Some(anchor),
            ViewportAnchor::Tail => None,
        };
        let distance_before = if let Some(anchor) = pinned_anchor {
            self.adapter
                .history_anchor_tail_distance(&replica.engine, anchor)
                .map_err(KernelError::Engine)?
        } else {
            None
        };
        self.engine_effects.clear();
        let applied =
            self.adapter
                .apply_output(&mut replica.engine, payload, &mut self.engine_effects);
        if let Err(error) = applied {
            let last_valid_seq = replica.last_seq;
            self.retire_after_codec_failure(terminal_id, generation, last_valid_seq, effects);
            return Err(KernelError::Engine(error));
        }
        if let Some(anchor) = pinned_anchor {
            Self::reconcile_pinned_anchor(
                &mut self.adapter,
                replica,
                anchor,
                distance_before,
                effects,
            );
        }
        replica.last_seq = seq;
        replica.next_seq = seq.checked_add(1);
        let replica_key = replica.key.clone();
        let acknowledge = matches!(
            replica_key.profile,
            BootstrapStreamProfile::SynthesizedVtStateSync
        );

        let damage_allowed = !self.attach_blocks(terminal_id);
        self.drain_engine_effects(&replica_key, damage_allowed, effects);
        if acknowledge {
            effects.push(KernelEffect::Send(KernelSend::FrameAck {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                seq,
            }));
        }
        Ok(())
    }
}
