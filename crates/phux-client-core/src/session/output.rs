//! The Terminal lane's publication of staged generations and live output.

use super::lane::{AttachSignal, LaneCx, LaneRegistry, OutputFrame};
use super::staging_rules::{fence_replaced, reject_retired, retire_for_codec_failure};
use super::{
    BootstrapStreamProfile, DocumentAnchorId, EffectBuffer, EngineAdapter, GenerationId,
    HistoryCache, HistoryUnavailableReason, KernelDamage, KernelDamageKind, KernelEffect,
    KernelError, KernelSend, KernelStatus, Replica, ResourceId, SessionKernel, TerminalState,
    ViewportAnchor, drain_engine_effects, generation_of, mismatch_error,
};

impl<R> TerminalState<R> {
    pub(super) fn publish<E: EngineAdapter<Replica = R>>(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        let terminal_id = cx.resource_id;
        let history_config = cx.history_config;
        {
            let staging = self
                .staging
                .as_mut()
                .ok_or_else(|| KernelError::MissingStaging(terminal_id.clone()))?;
            debug_assert!(staging.engine_ready && staging.protocol_ready);
            cx.adapter
                .configure_history_budget(
                    &mut staging.engine,
                    history_config.max_bytes,
                    history_config.max_materialized_rows,
                )
                .map_err(KernelError::Engine)?;
        }
        let mut staging = self
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
        if let Some(old) = self.published.replace(replacement) {
            fence_replaced(&mut self.retired, generation_of(&old.key), old.last_seq);
        }

        cx.attach = AttachSignal::Resolved;
        let damage_allowed = !cx.attach_blocks;
        for effect in pending_effects {
            SessionKernel::<E>::translate_engine_effect(
                &replica_key,
                effect,
                damage_allowed,
                effects,
            );
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

    /// The published replica for `generation`, or the error that says why
    /// it cannot accept the frame: the generation is retired or another
    /// generation is published.
    pub(super) fn published_for<X>(
        &mut self,
        terminal_id: &ResourceId,
        generation: GenerationId,
    ) -> Result<&mut Replica<R>, KernelError<X>> {
        reject_retired(&self.retired, terminal_id, generation)?;
        self.published
            .as_mut()
            .filter(|replica| generation_of(&replica.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))
    }

    /// Live output is exactly sequenced: the replica accepts `expected`
    /// and nothing else.
    const fn expect_next_seq<X>(replica: &Replica<R>, seq: u64) -> Result<(), KernelError<X>> {
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

    /// Retire the generation whose engine could not decode live output:
    /// drop the published replica, tombstone the generation, and ask the
    /// frontend for a resync.
    fn retire_after_codec_failure<E: EngineAdapter<Replica = R>>(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        generation: GenerationId,
        last_valid_seq: u64,
        effects: &mut EffectBuffer,
    ) {
        cx.engine_effects.clear();
        self.published = None;
        let damage_blocked = cx.attach_blocks;
        cx.attach = AttachSignal::Unresolved {
            pending_removal: damage_blocked,
        };
        retire_for_codec_failure(
            &mut self.retired,
            cx.resource_id,
            generation,
            last_valid_seq,
            effects,
        );
        if !damage_blocked {
            effects.push(KernelEffect::Damage(KernelDamage {
                terminal_id: cx.resource_id.clone(),
                kind: KernelDamageKind::Removed,
            }));
        }
    }

    pub(super) fn output<E: EngineAdapter<Replica = R>>(
        &mut self,
        cx: &mut LaneCx<'_, E>,
        frame: &OutputFrame<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        let terminal_id = cx.resource_id;
        crate::perf::OUTPUT_FRAMES.incr();
        crate::perf::OUTPUT_BYTES.add_len(frame.payload.len());
        cx.perf_echo.observe(terminal_id);
        let _timed = crate::perf::apply_timer();
        let generation = frame.generation;
        let seq = frame.seq;
        let replica = self.published_for(terminal_id, generation)?;
        Self::expect_next_seq(replica, seq)?;

        let pinned_anchor = match replica.history.viewport_anchor() {
            ViewportAnchor::Pinned(anchor) => Some(anchor),
            ViewportAnchor::Tail => None,
        };
        let distance_before = if let Some(anchor) = pinned_anchor {
            cx.adapter
                .history_anchor_tail_distance(&replica.engine, anchor)
                .map_err(KernelError::Engine)?
        } else {
            None
        };
        cx.engine_effects.clear();
        let applied =
            cx.adapter
                .apply_output(&mut replica.engine, frame.payload, cx.engine_effects);
        if let Err(error) = applied {
            let last_valid_seq = replica.last_seq;
            self.retire_after_codec_failure(cx, generation, last_valid_seq, effects);
            return Err(KernelError::Engine(error));
        }
        if let Some(anchor) = pinned_anchor {
            SessionKernel::<E>::reconcile_pinned_anchor(
                cx.adapter,
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

        let damage_allowed = !cx.attach_blocks;
        drain_engine_effects::<E>(cx.engine_effects, &replica_key, damage_allowed, effects);
        if acknowledge {
            effects.push(KernelEffect::Send(KernelSend::FrameAck {
                terminal_id: terminal_id.clone(),
                stream_id: generation.stream_id,
                bootstrap_id: generation.bootstrap_id,
                seq,
            }));
        }
        Ok(())
    }
}

impl<E: EngineAdapter> SessionKernel<E> {
    /// Resolve the published replica for `generation` in the Terminal lane,
    /// or the error that says why it cannot accept the frame.
    pub(super) fn published_replica<'a>(
        lanes: &'a mut LaneRegistry<E::Replica>,
        terminal_id: &ResourceId,
        generation: GenerationId,
    ) -> Result<&'a mut Replica<E::Replica>, KernelError<E::Error>> {
        lanes
            .terminal_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?
            .published_for(terminal_id, generation)
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
}
