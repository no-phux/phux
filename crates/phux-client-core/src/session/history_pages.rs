//! Progressive history pages, tombstones, and rejections.

use super::{
    BootstrapId, BootstrapProgress, BootstrapStreamProfile, EffectBuffer, EngineAdapter,
    EngineEffectBuffer, GenerationId, HistoryCacheError, HistoryCursor, HistoryLoadState,
    HistoryPageCheck, HistoryPageInput, HistoryPageRejection, HistoryRejectionReason,
    HistoryStatus, HistoryUnavailableReason, KernelEffect, KernelError, KernelSend, KernelStatus,
    Replica, ReplicaKey, ResourceId, SessionKernel, StreamId, generation_of, mismatch_error,
    retired_error,
};

impl<E: EngineAdapter> SessionKernel<E> {
    /// Import one already-validated page into the engine and then into
    /// the history cache. On failure the caller invalidates progressive
    /// history with the returned reason.
    fn import_history_page(
        adapter: &mut E,
        engine_effects: &mut EngineEffectBuffer,
        replica: &mut Replica<E::Replica>,
        page: &HistoryPageInput<'_>,
        cursor: &HistoryCursor,
        next_cursor: Option<HistoryCursor>,
    ) -> Result<(), HistoryPageRejection<E::Error>> {
        engine_effects.clear();
        let outcome = adapter
            .apply_history_page(&mut replica.engine, page.payload, page.rows, engine_effects)
            .map_err(|error| HistoryPageRejection::codec_failure(KernelError::Engine(error)))?;

        let has_more = next_cursor.is_some();
        let native = matches!(
            replica.key.profile,
            BootstrapStreamProfile::NativeState { .. }
        );
        let finished = matches!(outcome.progress, BootstrapProgress::Finished);
        if native && finished == has_more {
            return Err(HistoryPageRejection::codec_failure(
                KernelError::HistoryCompletionMismatch {
                    progress: outcome.progress,
                    has_more,
                },
            ));
        }
        let authenticated_rows = u32::try_from(outcome.authenticated_rows).map_err(|_| {
            HistoryPageRejection::codec_failure(KernelError::HistoryCompletionMismatch {
                progress: outcome.progress,
                has_more,
            })
        })?;
        replica.history.commit_checked_page(
            cursor,
            page.page_seq,
            next_cursor,
            authenticated_rows,
            outcome.retained_rows,
            page.payload,
        );
        Ok(())
    }

    /// Report the cache's current history status. This is the whole
    /// response to a byte-identical duplicate page.
    fn report_history_status(replica: &Replica<E::Replica>, effects: &mut EffectBuffer) {
        effects.push(KernelEffect::Status(KernelStatus::History {
            key: replica.key.clone(),
            status: replica.history.status(),
        }));
    }

    /// Emit the status, and the follow-on request, that a successfully
    /// imported page produces.
    fn finish_history_page(
        &mut self,
        terminal_id: &ResourceId,
        replica_key: ReplicaKey,
        history_status: HistoryStatus,
        next_request: Option<HistoryCursor>,
        effects: &mut EffectBuffer,
    ) {
        let damage_allowed = !self.attach_blocks(terminal_id);
        self.drain_engine_effects(&replica_key, damage_allowed, effects);
        effects.push(KernelEffect::Status(KernelStatus::History {
            key: replica_key.clone(),
            status: history_status,
        }));
        if let Some(cursor) = next_request {
            effects.push(KernelEffect::Send(KernelSend::HistoryRequest {
                key: replica_key,
                cursor: cursor.as_bytes().to_vec(),
                max_bytes: self.history_config.request_max_bytes,
                max_rows: self.history_config.request_max_rows,
            }));
        }
    }

    pub(super) fn history_page(
        &mut self,
        page: &HistoryPageInput<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(page.terminal_id)?;
        let replica =
            Self::published_replica(&mut self.terminals, page.terminal_id, page.generation)?;
        if replica.history.is_locally_cleared() {
            return Ok(());
        }
        let cursor = HistoryCursor::new(page.cursor);
        let next_cursor = page.next_cursor.map(HistoryCursor::new);
        match replica.history.check_page(
            &cursor,
            page.page_seq,
            next_cursor.as_ref(),
            page.rows,
            page.payload,
        ) {
            Ok(HistoryPageCheck::Duplicate(_)) => {
                Self::report_history_status(replica, effects);
                return Ok(());
            }
            Ok(HistoryPageCheck::New) => {}
            Err(error) => {
                // Cursor/sequence failures invalidate only progressive history.
                // The published live replica remains authoritative and usable.
                Self::tombstone_history(
                    &mut self.adapter,
                    replica,
                    HistoryUnavailableReason::CodecFailure,
                    effects,
                );
                return Err(KernelError::HistoryCache(error));
            }
        }

        if let Err(rejection) = Self::import_history_page(
            &mut self.adapter,
            &mut self.engine_effects,
            replica,
            page,
            &cursor,
            next_cursor,
        ) {
            self.engine_effects.clear();
            Self::tombstone_history(&mut self.adapter, replica, rejection.reason, effects);
            return rejection.error.map_or_else(|| Ok(()), Err);
        }

        let next_request = replica
            .history
            .should_auto_continue()
            .then(|| replica.history.begin_fetch())
            .flatten();
        let replica_key = replica.key.clone();
        let history_status = replica.history.status();
        self.finish_history_page(
            page.terminal_id,
            replica_key,
            history_status,
            next_request,
            effects,
        );
        Ok(())
    }

    pub(super) fn history_tombstone(
        &mut self,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        cursor: &[u8],
        reason: HistoryUnavailableReason,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let state = self
            .terminals
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        if state.retired.contains_key(&generation) {
            return Err(retired_error(terminal_id, generation));
        }
        let replica = state
            .published
            .as_mut()
            .filter(|replica| generation_of(&replica.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        let cursor = HistoryCursor::new(cursor);
        let state = match reason {
            HistoryUnavailableReason::Stale => HistoryLoadState::Stale,
            HistoryUnavailableReason::Pruned => HistoryLoadState::Pruned,
            HistoryUnavailableReason::Reset
            | HistoryUnavailableReason::Resize
            | HistoryUnavailableReason::Expired
            | HistoryUnavailableReason::Released
            | HistoryUnavailableReason::Limit
            | HistoryUnavailableReason::CodecFailure => HistoryLoadState::Tombstoned,
        };
        if !replica.history.invalidate_cursor(&cursor, state) {
            // A tombstone for an already-advanced cursor is stale; history is
            // optional, so ignore it rather than fail a healthy live pane.
            return Ok(());
        }
        self.adapter.clear_document_state(&mut replica.engine);
        effects.push(KernelEffect::Status(KernelStatus::HistoryUnavailable {
            key: replica.key.clone(),
            reason,
        }));
        effects.push(KernelEffect::Status(KernelStatus::History {
            key: replica.key.clone(),
            status: replica.history.status(),
        }));
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn history_rejected(
        &mut self,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        cursor: &[u8],
        reason: HistoryRejectionReason,
        required_bytes: u32,
        required_rows: u32,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let state = self
            .terminals
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        if state.retired.contains_key(&generation) {
            return Err(retired_error(terminal_id, generation));
        }
        let replica = state
            .published
            .as_mut()
            .filter(|replica| generation_of(&replica.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        let cursor = HistoryCursor::new(cursor);
        let next_request =
            Self::retry_rejected_history(replica, &cursor, reason, required_bytes, required_rows)
                .map_err(KernelError::HistoryCache)?;
        if matches!(
            reason,
            HistoryRejectionReason::TooSmall | HistoryRejectionReason::Busy
        ) && next_request.is_none()
        {
            replica.history.tombstone();
            self.adapter.clear_document_state(&mut replica.engine);
            effects.push(KernelEffect::Status(KernelStatus::HistoryUnavailable {
                key: replica.key.clone(),
                reason: HistoryUnavailableReason::Limit,
            }));
        }
        effects.push(KernelEffect::Status(KernelStatus::History {
            key: replica.key.clone(),
            status: replica.history.status(),
        }));
        if let Some((cursor, max_bytes, max_rows)) = next_request {
            effects.push(KernelEffect::Send(KernelSend::HistoryRequest {
                key: replica.key.clone(),
                cursor: cursor.as_bytes().to_vec(),
                max_bytes,
                max_rows,
            }));
        }
        Ok(())
    }

    fn retry_rejected_history(
        replica: &mut Replica<E::Replica>,
        cursor: &HistoryCursor,
        reason: HistoryRejectionReason,
        required_bytes: u32,
        required_rows: u32,
    ) -> Result<Option<(HistoryCursor, u32, u32)>, HistoryCacheError> {
        let retry_limits = (reason == HistoryRejectionReason::TooSmall)
            .then(|| replica.history.retry_limits(required_bytes, required_rows))
            .flatten();
        let retry_busy = (reason == HistoryRejectionReason::Busy
            && replica.history.allow_busy_retry())
        .then(|| replica.history.request_limits());
        if !replica.history.cancel_fetch(cursor) {
            return Err(HistoryCacheError::Gap);
        }
        if let Some((max_bytes, max_rows)) = retry_limits {
            return Ok(replica
                .history
                .begin_fetch_with_limits(max_bytes, max_rows)
                .map(|cursor| (cursor, max_bytes, max_rows)));
        }
        let Some((max_bytes, max_rows)) = retry_busy else {
            return Ok(None);
        };
        Ok(replica
            .history
            .begin_fetch_with_limits(max_bytes, max_rows)
            .map(|cursor| (cursor, max_bytes, max_rows)))
    }
}
