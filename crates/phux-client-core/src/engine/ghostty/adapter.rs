//! The [`EngineAdapter`] and [`EnginePresentationAdapter`] implementations.

use super::{
    BootstrapProgress, BootstrapStreamProfile, CONTINUATION_LIMIT, CanonicalGeometry,
    DocumentAnchorId, EngineAdapter, EngineCodec, EngineDamage, EngineEffect, EngineEffectBuffer,
    EnginePresentationAdapter, FeedDecoder, GhosttyAdapter, GhosttyEngineError, GhosttyReplica,
    GhosttyTerminal, HashMap, HistoryApplyOutcome, NativeDecoderState, NativeReplica,
    PendingEffects, PhantomData, PointSpace, Rc, ReplicaState, SYNTH_SCROLLBACK_ROWS,
    enforce_history_budget, finish_native, install_effect_hooks, push_history, push_native,
};

impl EngineAdapter for GhosttyAdapter {
    type Replica = GhosttyReplica;
    type Error = GhosttyEngineError;

    fn start_replica(
        &mut self,
        profile: BootstrapStreamProfile,
        geometry: CanonicalGeometry,
    ) -> Result<Self::Replica, Self::Error> {
        let pending = Rc::new(PendingEffects::default());
        let state = match profile {
            BootstrapStreamProfile::SynthesizedVtRaw
            | BootstrapStreamProfile::SynthesizedVtStateSync => {
                let mut terminal = GhosttyTerminal::new(geometry.cols, geometry.rows)?;
                terminal.set_scrollback_max_lines(Some(SYNTH_SCROLLBACK_ROWS))?;
                let _ = terminal.set_continuation_max_bytes(CONTINUATION_LIMIT);
                install_effect_hooks(&mut terminal, &pending)?;
                ReplicaState::Synthesized {
                    terminal,
                    protocol_finished: false,
                }
            }
            BootstrapStreamProfile::NativeState {
                codec: EngineCodec::LibghosttySnapshotV1,
            } if self.native_available => {
                let mut decoder = FeedDecoder::new(CONTINUATION_LIMIT)?;
                decoder.set_max_continuation_bytes(CONTINUATION_LIMIT)?;
                ReplicaState::Native(NativeReplica {
                    pending: Rc::clone(&pending),
                    decoder: NativeDecoderState::Collecting(decoder),
                    protocol_finished: false,
                })
            }
            _ => return Err(GhosttyEngineError::UnsupportedProfile(profile)),
        };
        Ok(GhosttyReplica {
            pending,
            reported_title: None,
            profile,
            state,
            anchors: HashMap::new(),
            history_max_bytes: None,
            history_max_lines: None,
            _not_send_or_sync: PhantomData,
        })
    }

    fn configure_history_budget(
        &mut self,
        replica: &mut Self::Replica,
        max_bytes: usize,
        max_rows: usize,
    ) -> Result<(), Self::Error> {
        replica.history_max_bytes = Some(max_bytes.max(1));
        replica.history_max_lines = Some(max_rows.max(1));
        replica.set_scrollback_max_bytes(replica.history_max_bytes)?;
        replica.set_scrollback_max_lines(replica.history_max_lines)?;
        Ok(())
    }

    fn clear_document_state(&mut self, replica: &mut Self::Replica) {
        replica.anchors.clear();
    }

    fn history_anchor_tail_distance(
        &self,
        replica: &Self::Replica,
        anchor: DocumentAnchorId,
    ) -> Result<Option<u64>, Self::Error> {
        let Some(anchor) = replica.anchors.get(&anchor) else {
            return Ok(None);
        };
        let Some(point) = anchor.point(PointSpace::History)? else {
            return Ok(None);
        };
        let rows = replica
            .terminal()
            .ok_or(GhosttyEngineError::LiveOutputBeforeReady)?
            .scrollback_rows()?;
        let y = point.y as usize;
        if y >= rows {
            return Ok(None);
        }
        Ok(Some(
            u64::try_from(rows.saturating_sub(y).saturating_sub(1)).unwrap_or(u64::MAX),
        ))
    }

    fn apply_bootstrap_chunk(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        _effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        let limit = self.limits.max_chunk_bytes() as usize;
        if payload.len() > limit {
            return Err(GhosttyEngineError::PayloadLimitExceeded {
                actual: payload.len(),
                limit,
            });
        }
        let progress = match &mut replica.state {
            ReplicaState::Synthesized {
                terminal,
                protocol_finished,
            } => {
                if *protocol_finished {
                    return Err(GhosttyEngineError::InputAfterFinish);
                }
                terminal.vt_write(payload);
                BootstrapProgress::Pending
            }
            ReplicaState::Native(native) => push_native(native, payload)?,
        };
        enforce_history_budget(replica)?;
        Ok(progress)
    }

    fn bootstrap_staging_bytes(&self, replica: &Self::Replica) -> usize {
        match &replica.state {
            ReplicaState::Native(native) => match &native.decoder {
                NativeDecoderState::Collecting(decoder) => decoder.staged_capacity_bytes(),
                NativeDecoderState::Ready(decoder) => decoder.staged_capacity_bytes(),
                NativeDecoderState::Finished(_) | NativeDecoderState::Failed => 0,
            },
            ReplicaState::Synthesized { .. } => 0,
        }
    }

    fn finish_bootstrap(
        &mut self,
        replica: &mut Self::Replica,
        effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        let progress = match &mut replica.state {
            ReplicaState::Synthesized {
                protocol_finished, ..
            } => {
                if std::mem::replace(protocol_finished, true) {
                    return Err(GhosttyEngineError::InputAfterFinish);
                }
                BootstrapProgress::Finished
            }
            ReplicaState::Native(native) => finish_native(native)?,
        };
        if let Some(theme) = &self.theme {
            replica.apply_theme(Some(theme))?;
        }
        enforce_history_budget(replica)?;
        replica.publish_title(effects)?;
        // Synthesized bootstrap bytes are history, not new attention events
        // or clipboard writes (ADR-0158: live output only).
        replica.pending.clear();
        Ok(progress)
    }

    fn apply_history_page(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        declared_rows: u32,
        _effects: &mut EngineEffectBuffer,
    ) -> Result<HistoryApplyOutcome, Self::Error> {
        let limit = self.limits.max_history_page_bytes() as usize;
        if payload.len() > limit {
            return Err(GhosttyEngineError::PayloadLimitExceeded {
                actual: payload.len(),
                limit,
            });
        }
        let profile = replica.profile;
        let progress = match &mut replica.state {
            ReplicaState::Synthesized { .. } => {
                return Err(GhosttyEngineError::HistoryUnsupported(profile));
            }
            ReplicaState::Native(native) => push_history(native, payload, declared_rows)?,
        };
        enforce_history_budget(replica)?;
        Ok(progress)
    }

    fn apply_output(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<(), Self::Error> {
        match &mut replica.state {
            ReplicaState::Native(native) if !native.protocol_finished => {
                return Err(GhosttyEngineError::LiveOutputBeforeReady);
            }
            ReplicaState::Synthesized { terminal, .. } => terminal.vt_write(payload),
            ReplicaState::Native(native) => match &mut native.decoder {
                NativeDecoderState::Ready(decoder) => decoder.terminal_mut().vt_write(payload),
                NativeDecoderState::Finished(terminal) => terminal.vt_write(payload),
                NativeDecoderState::Collecting(_) | NativeDecoderState::Failed => {
                    return Err(GhosttyEngineError::LiveOutputBeforeReady);
                }
            },
        }
        replica.publish_title(effects)?;
        if replica.pending.bell.replace(false) {
            effects.push(EngineEffect::Status(crate::engine::EngineStatus::Bell));
        }
        if let Some(text) = replica.pending.clipboard.take() {
            effects.push(EngineEffect::Status(
                crate::engine::EngineStatus::ClipboardWrite(crate::engine::ClipboardText(text)),
            ));
        }
        effects.push(EngineEffect::Damage(EngineDamage::Full));
        Ok(())
    }
}

impl EnginePresentationAdapter for GhosttyAdapter {
    fn clear_presentation(&mut self, replica: &mut Self::Replica) -> Result<(), Self::Error> {
        replica.clear_presentation()?;
        self.clear_document_state(replica);
        Ok(())
    }
}
