//! Native history paging.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

impl TerminalActor {
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn handle_native_publication(&mut self, req: NativePublicationRequest) {
        let key = NativeCursorKey::new(req.owner, req.stream_id);
        let valid = self.native_cursor_owners.get(&key).is_some_and(|binding| {
            binding.cursor == req.cursor
                && binding.terminal_id == req.terminal_id
                && binding.stream_id == req.stream_id
                && binding.bootstrap_id == req.bootstrap_id
        });
        if !valid {
            let _ = req
                .reply
                .send(Err(crate::native_state::NativeStateError::InvalidHandle));
            return;
        }
        let Some(publication) = self.native_publications.get_mut(&req.cursor) else {
            let _ = req
                .reply
                .send(Err(crate::native_state::NativeStateError::InvalidHandle));
            return;
        };
        if !publication.waiting.remove(&key) {
            let _ = req
                .reply
                .send(Err(crate::native_state::NativeStateError::InvalidHandle));
            return;
        }
        let replay = publication.replay.iter().cloned().collect();
        let live = self.core.output_tx.subscribe();
        let remove = publication.waiting.is_empty();
        if remove {
            self.native_publications.remove(&req.cursor);
        }
        let _ = req.reply.send(Ok(NativePublicationReply { replay, live }));
    }

    /// Queue one HISTORY request for bounded cooperative capture.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn handle_native_history(&mut self, req: NativeHistoryRequest) {
        let pending = PendingNativeHistory {
            request: req,
            started_at: tokio::time::Instant::now(),
        };
        if self.pending_native_history.is_none() {
            self.pending_native_history = Some(pending);
            return;
        }
        if self.native_history_backlog.len() < MAX_NATIVE_HISTORY_CLIENTS.saturating_sub(1) {
            self.native_history_backlog.push_back(pending);
            return;
        }
        let req = pending.request;
        let row_bound = req.max_rows.clamp(1, phux_protocol::MAX_HISTORY_PAGE_ROWS);
        let byte_bound = req
            .max_bytes
            .min(req.limits.max_history_page_bytes())
            .max(1);
        let id = HistoryFrameId {
            terminal_id: req.terminal_id,
            stream_id: req.stream_id,
            bootstrap_id: req.bootstrap_id,
            cursor: req.cursor,
        };
        answer_history(
            req.reply,
            req.permit,
            Ok(id.rejected(
                phux_protocol::wire::frame::HistoryRejectionReason::Busy,
                byte_bound,
                row_bound,
            )),
        );
    }

    /// Advance at most one engine history step for the oldest queued request.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn step_native_history(&mut self) {
        let Some(pending) = self.pending_native_history.take() else {
            return;
        };
        if pending.started_at.elapsed() > NATIVE_CAPTURE_LIFETIME {
            let request = pending.request;
            let key = NativeCursorKey::new(request.owner, request.stream_id);
            let frame = history_request_id(&request)
                .tombstone(phux_protocol::wire::frame::HistoryTombstoneReason::Expired);
            self.release_native_binding(key);
            answer_history(request.reply, request.permit, Ok(frame));
            self.start_next_native_history();
            return;
        }
        match self.native_history_frame(&pending.request) {
            Ok(Some(frame)) => {
                let request = pending.request;
                let key = NativeCursorKey::new(request.owner, request.stream_id);
                let keep = self.native_cursor_owners.contains_key(&key);
                if request
                    .reply
                    .send(NativeHistoryReply {
                        permit: request.permit,
                        result: Ok(frame),
                    })
                    .is_err()
                    && keep
                {
                    self.release_native_binding(key);
                }
                self.start_next_native_history();
            }
            Ok(None) => self.pending_native_history = Some(pending),
            Err(error) => {
                let request = pending.request;
                answer_history(request.reply, request.permit, Err(error));
                self.start_next_native_history();
            }
        }
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn start_next_native_history(&mut self) {
        // Completion can release an owner, which already promotes the next
        // request through cancellation cleanup. Never overwrite that owner.
        if self.pending_native_history.is_none() {
            self.pending_native_history = self.native_history_backlog.pop_front();
        }
    }

    /// Validate and advance one request. `None` means the engine made bounded
    /// progress but has not completed the next authenticated unit yet.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn native_history_frame(
        &mut self,
        req: &NativeHistoryRequest,
    ) -> Result<Option<FrameKind>, crate::native_state::NativeStateError> {
        let id = history_request_id(req);
        // An unusable cursor is a routine race (a resize drained the
        // bindings while the request was in flight), answered with a
        // HISTORY_TOMBSTONE rather than a connection error.
        let stale = phux_protocol::wire::frame::HistoryTombstoneReason::Stale;
        let Ok(cursor): Result<crate::native_state::OpaqueHistoryCursor, _> =
            id.cursor.as_ref().try_into()
        else {
            // A protocol violation, worth a log line but not the attach.
            warn!(
                len = id.cursor.len(),
                terminal_id = ?id.terminal_id,
                "HISTORY_REQUEST carried a malformed cursor"
            );
            return Ok(Some(id.tombstone(stale)));
        };
        let Some((page_seq, record_index)) =
            self.history_binding(NativeCursorKey::new(req.owner, req.stream_id), &id, cursor)
        else {
            return Ok(Some(id.tombstone(stale)));
        };
        let bound = req.max_bytes.min(req.limits.max_history_page_bytes());
        if bound == 0 || req.max_rows == 0 {
            let frame = id.rejected(
                phux_protocol::wire::frame::HistoryRejectionReason::ZeroLimit,
                1,
                1,
            );
            return Ok(Some(frame));
        }
        let row_bound = req.max_rows.min(phux_protocol::MAX_HISTORY_PAGE_ROWS);
        let frame = match self.history_record_at(&cursor, record_index, bound, row_bound) {
            Ok(record) => {
                let finish = record.finish;
                let rows = self.advance_history_binding(
                    NativeCursorKey::new(req.owner, req.stream_id),
                    &record,
                    page_seq,
                )?;
                id.page(page_seq, record.bytes, rows, finish)
            }
            Err(crate::native_state::NativeStateError::OutOfSpace {
                required_bytes,
                required_rows,
            }) => {
                self.history_out_of_space(
                    NativeCursorKey::new(req.owner, req.stream_id),
                    id,
                    req.limits,
                    required_bytes,
                    required_rows,
                )
                .0
            }
            Err(crate::native_state::NativeStateError::ImportBusy) => return Ok(None),
            Err(error) => {
                let reason = history_tombstone_reason(error);
                self.release_native_binding(NativeCursorKey::new(req.owner, req.stream_id));
                id.tombstone(reason)
            }
        };
        Ok(Some(frame))
    }

    /// Resolve this pump's cursor binding, returning its next page sequence and
    /// record index. `None` when the pump has no binding or the binding does
    /// not match this request's generation identity -- both routine races the
    /// caller answers with a stale tombstone.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn history_binding(
        &self,
        key: NativeCursorKey,
        id: &HistoryFrameId,
        cursor: crate::native_state::OpaqueHistoryCursor,
    ) -> Option<(u64, usize)> {
        let binding = self.native_cursor_owners.get(&key)?;
        if binding.cursor != cursor
            || binding.terminal_id != id.terminal_id
            || binding.stream_id != id.stream_id
            || binding.bootstrap_id != id.bootstrap_id
        {
            return None;
        }
        Some((binding.next_page_seq, binding.record_index))
    }

    /// Pull one history record out of the native manager, bounded by the
    /// caller's byte and row budget. A host with no native manager surfaces
    /// its own error unchanged.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn history_record_at(
        &self,
        cursor: &crate::native_state::OpaqueHistoryCursor,
        record_index: usize,
        bound: u32,
        max_rows: u32,
    ) -> Result<crate::native_state::CachedNativeHistoryRecord, crate::native_state::NativeStateError>
    {
        let mut host = self.terminal.borrow_mut();
        match host.native_manager() {
            Ok(manager) => manager.history_record_at(cursor, record_index, bound, max_rows),
            Err(error) => Err(error),
        }
    }

    /// Advance this pump's binding past the record just served, returning the
    /// page's row count. A binding that can no longer be advanced (sequence or
    /// row count out of range) releases the pump and fails the request; a
    /// finished record releases the pump too, since there is no next page.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn advance_history_binding(
        &mut self,
        key: NativeCursorKey,
        record: &crate::native_state::CachedNativeHistoryRecord,
        page_seq: u64,
    ) -> Result<u32, crate::native_state::NativeStateError> {
        let Some(next_page_seq) = page_seq.checked_add(1) else {
            self.release_native_binding(key);
            return Err(crate::native_state::NativeStateError::LimitExceeded);
        };
        let Ok(rows) = u32::try_from(record.rows) else {
            self.release_native_binding(key);
            return Err(crate::native_state::NativeStateError::LimitExceeded);
        };
        if record.finish {
            self.release_native_binding(key);
        } else if let Some(binding) = self.native_cursor_owners.get_mut(&key) {
            binding.record_index += 1;
            binding.next_page_seq = next_page_seq;
            binding.touched = tokio::time::Instant::now();
        }
        Ok(rows)
    }

    /// Answer a record the caller's buffer cannot hold: `HISTORY_REJECTED` with
    /// the exact requirement when the client can retry within the negotiated
    /// bounds, else a `HISTORY_TOMBSTONE` that also releases the pump. Returns
    /// the frame and whether the pump is still bound.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn history_out_of_space(
        &mut self,
        key: NativeCursorKey,
        id: HistoryFrameId,
        limits: phux_protocol::caps::BootstrapLimits,
        required_bytes: usize,
        required_rows: usize,
    ) -> (FrameKind, bool) {
        let frame = match (
            u32::try_from(required_bytes),
            u32::try_from(required_rows.max(1)),
        ) {
            (Ok(required_bytes), Ok(required_rows))
                if required_bytes != 0
                    && required_bytes <= limits.max_history_page_bytes()
                    && required_rows <= phux_protocol::MAX_HISTORY_PAGE_ROWS =>
            {
                id.rejected(
                    phux_protocol::wire::frame::HistoryRejectionReason::TooSmall,
                    required_bytes,
                    required_rows,
                )
            }
            _ => {
                self.release_native_binding(key);
                id.tombstone(phux_protocol::wire::frame::HistoryTombstoneReason::Limit)
            }
        };
        (frame, self.native_cursor_owners.contains_key(&key))
    }
}

/// Answer every still-live prepared waiter with `error`.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(in crate::actor) fn fail_native_waiters(
    prepared: Vec<(NativeBootstrapRequest, NativeBootstrapReply)>,
    error: crate::native_state::NativeStateError,
) {
    for (waiter, _) in prepared {
        let _ = waiter.reply.send(Err(error));
    }
}

/// Keep waiters that are not the targeted pump. `stream_id = None` drops every
/// waiter for `owner` (client detach); `Some` drops only that stream.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(in crate::actor) fn keeps_native_bootstrap_waiter(
    waiter: &NativeBootstrapRequest,
    owner: u64,
    stream_id: Option<phux_protocol::ids::StreamId>,
) -> bool {
    waiter.owner != owner || stream_id.is_some_and(|sid| waiter.stream_id != sid)
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(in crate::actor) fn matches_native_history(
    request: &NativeHistoryRequest,
    owner: u64,
    stream_id: Option<phux_protocol::ids::StreamId>,
) -> bool {
    request.owner == owner && stream_id.is_none_or(|sid| request.stream_id == sid)
}

/// The wire identity every HISTORY answer frame is stamped with.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(in crate::actor) struct HistoryFrameId {
    /// Wire terminal identity for this subscription.
    terminal_id: phux_protocol::ids::ResourceId,
    /// Logical stream identity.
    stream_id: phux_protocol::ids::StreamId,
    /// Replica generation identity.
    bootstrap_id: phux_protocol::ids::BootstrapId,
    /// The opaque cursor the client echoed.
    cursor: Bytes,
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(in crate::actor) fn history_request_id(request: &NativeHistoryRequest) -> HistoryFrameId {
    HistoryFrameId {
        terminal_id: request.terminal_id.clone(),
        stream_id: request.stream_id,
        bootstrap_id: request.bootstrap_id,
        cursor: request.cursor.clone(),
    }
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
impl HistoryFrameId {
    /// Stamp a `HISTORY_TOMBSTONE`: this cursor will never be served again.
    pub(in crate::actor) fn tombstone(
        self,
        reason: phux_protocol::wire::frame::HistoryTombstoneReason,
    ) -> FrameKind {
        FrameKind::HistoryTombstone {
            terminal_id: self.terminal_id,
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            cursor: self.cursor,
            reason,
        }
    }

    /// Stamp a `HISTORY_REJECTED`: the request as posed cannot be served, but
    /// the cursor stays usable for a retry within the stated requirement.
    pub(in crate::actor) fn rejected(
        self,
        reason: phux_protocol::wire::frame::HistoryRejectionReason,
        required_bytes: u32,
        required_rows: u32,
    ) -> FrameKind {
        FrameKind::HistoryRejected {
            terminal_id: self.terminal_id,
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            cursor: self.cursor,
            reason,
            required_bytes,
            required_rows,
        }
    }

    /// Stamp a `HISTORY_PAGE`, echoing the cursor as `next_cursor` while more
    /// records remain.
    pub(in crate::actor) fn page(
        self,
        page_seq: u64,
        payload: Bytes,
        rows: u32,
        finish: bool,
    ) -> FrameKind {
        FrameKind::HistoryPage {
            terminal_id: self.terminal_id,
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            page_seq,
            cursor: self.cursor.clone(),
            next_cursor: (!finish).then_some(self.cursor),
            payload,
            rows,
        }
    }
}

/// Answer one HISTORY request, handing the reserved permit back to the pump.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(in crate::actor) fn answer_history(
    reply: tokio::sync::oneshot::Sender<NativeHistoryReply>,
    permit: tokio::sync::mpsc::OwnedPermit<phux_resource::mailbox::Outbound>,
    result: Result<FrameKind, crate::native_state::NativeStateError>,
) {
    let _ = reply.send(NativeHistoryReply { permit, result });
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(in crate::actor) fn answer_released_history(request: NativeHistoryRequest) {
    answer_tombstoned_history(
        request,
        phux_protocol::wire::frame::HistoryTombstoneReason::Released,
    );
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(in crate::actor) fn answer_tombstoned_history(
    request: NativeHistoryRequest,
    reason: phux_protocol::wire::frame::HistoryTombstoneReason,
) {
    let frame = history_request_id(&request).tombstone(reason);
    answer_history(request.reply, request.permit, Ok(frame));
}

/// Map a native-state failure onto the tombstone reason the wire carries.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(in crate::actor) const fn history_tombstone_reason(
    error: crate::native_state::NativeStateError,
) -> phux_protocol::wire::frame::HistoryTombstoneReason {
    match error {
        crate::native_state::NativeStateError::Stale
        | crate::native_state::NativeStateError::WrongGeneration => {
            phux_protocol::wire::frame::HistoryTombstoneReason::Stale
        }
        crate::native_state::NativeStateError::Pruned => {
            phux_protocol::wire::frame::HistoryTombstoneReason::Pruned
        }
        crate::native_state::NativeStateError::Reset => {
            phux_protocol::wire::frame::HistoryTombstoneReason::Reset
        }
        crate::native_state::NativeStateError::Resize => {
            phux_protocol::wire::frame::HistoryTombstoneReason::Resize
        }
        crate::native_state::NativeStateError::LimitExceeded => {
            phux_protocol::wire::frame::HistoryTombstoneReason::Limit
        }
        _ => phux_protocol::wire::frame::HistoryTombstoneReason::CodecFailure,
    }
}
