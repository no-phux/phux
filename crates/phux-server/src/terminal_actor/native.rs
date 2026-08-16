//! Native-engine request handling for [`TerminalActor`]: bootstrap
//! capture, publication handoff, history paging, cursor-owner
//! bookkeeping, and live-output replay buffering.

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
use super::{
    Bytes, CanonicalTerminal, FrameKind, HashSet, MAX_NATIVE_HISTORY_CLIENTS,
    MAX_NATIVE_REPLAY_BYTES, NATIVE_HISTORY_TTL, NativeBootstrapReply, NativeBootstrapRequest,
    NativeCursorOwner, NativeHistoryReply, NativeHistoryRequest, NativePublicationGeneration,
    NativePublicationReply, NativePublicationRequest, PaneOutput, PendingNativeBootstrap, VecDeque,
    native_step_bytes, reserve_native_bytes, warn,
};
use super::{NativeActorRequest, TerminalActor};

impl TerminalActor {
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn handle_native_bootstrap(&mut self, req: NativeBootstrapRequest) {
        let owner = req.owner;
        self.invalidate_native_owner(
            owner,
            phux_protocol::wire::frame::TombstoneReason::ExplicitReattach,
        );
        if let Some(pending) = self.pending_native_bootstrap.as_mut() {
            if pending.limits == req.limits
                && pending.capture_bytes
                    == req
                        .max_bytes
                        .saturating_sub(phux_protocol::wire::frame::MAX_HISTORY_CURSOR_BYTES)
                && pending.max_chunks == req.max_frames.saturating_sub(2)
                && pending.waiters.len() < MAX_NATIVE_HISTORY_CLIENTS
            {
                pending.waiters.retain(|waiter| waiter.owner != owner);
                pending.waiters.push(req);
                return;
            }
            self.native_bootstrap_backlog
                .retain(|waiter| waiter.owner != owner);
            self.native_bootstrap_backlog.push_back(req);
            return;
        }
        self.start_native_bootstrap(req);
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn start_native_bootstrap(&mut self, req: NativeBootstrapRequest) {
        let limits = req.limits;
        let chunk_bytes = match usize::try_from(limits.max_chunk_bytes()) {
            Ok(size) if size != 0 => size.min(req.max_bytes),
            _ => {
                let _ = req
                    .reply
                    .send(Err(crate::native_state::NativeStateError::LimitExceeded));
                return;
            }
        };
        let Some(max_chunks) = req.max_frames.checked_sub(2) else {
            let _ = req
                .reply
                .send(Err(crate::native_state::NativeStateError::LimitExceeded));
            return;
        };
        let Some(capture_bytes) = req
            .max_bytes
            .checked_sub(phux_protocol::wire::frame::MAX_HISTORY_CURSOR_BYTES)
        else {
            let _ = req
                .reply
                .send(Err(crate::native_state::NativeStateError::LimitExceeded));
            return;
        };
        if max_chunks == 0 || capture_bytes == 0 {
            let _ = req
                .reply
                .send(Err(crate::native_state::NativeStateError::LimitExceeded));
            return;
        }
        let capture = {
            let mut host = self.terminal.borrow_mut();
            match host.native_manager().and_then(|manager| {
                manager.begin_generation_capture(limits, capture_bytes, max_chunks)
            }) {
                Ok(capture) => capture,
                Err(error) => {
                    let _ = req.reply.send(Err(error));
                    return;
                }
            }
        };
        let scratch_bytes = match native_step_bytes(capture_bytes, 0, capture.max_record_bytes()) {
            Ok(bytes) => bytes,
            Err(error) => {
                if let CanonicalTerminal::Native(manager) = &mut *self.terminal.borrow_mut() {
                    manager.abort_generation_capture(capture);
                }
                let _ = req.reply.send(Err(error));
                return;
            }
        };
        let mut scratch = match reserve_native_bytes(scratch_bytes) {
            Ok(scratch) => scratch,
            Err(error) => {
                if let CanonicalTerminal::Native(manager) = &mut *self.terminal.borrow_mut() {
                    manager.abort_generation_capture(capture);
                }
                let _ = req.reply.send(Err(error));
                return;
            }
        };
        scratch.resize(scratch_bytes, 0);
        self.pending_native_bootstrap = Some(PendingNativeBootstrap {
            capture: Some(capture),
            waiters: vec![req],
            records: Vec::new(),
            retained_bytes: 0,
            capture_bytes,
            scratch,
            max_chunks,
            chunk_bytes,
            base_seq: self.raw_seq,
            chunk_count: 0,
            limits,
            replay: VecDeque::new(),
            replay_bytes: 0,
        });
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    #[allow(
        clippy::too_many_lines,
        reason = "one cooperative step of the bootstrap state machine: each arm advances the same `pending` value to the next state and they share the buffer-growth retry, so splitting on an arm boundary would thread that scratch buffer and the pending record through a second signature for no reader benefit"
    )]
    pub(super) fn step_native_bootstrap(&mut self) {
        let Some(mut pending) = self.pending_native_bootstrap.take() else {
            return;
        };
        let step_bytes = match native_step_bytes(
            pending.capture_bytes,
            pending.retained_bytes,
            pending.capture.as_ref().map_or(
                0,
                crate::native_state::NativeManagedCapture::max_record_bytes,
            ),
        ) {
            Ok(bytes) => bytes.min(pending.scratch.len()),
            Err(error) => {
                self.fail_native_bootstrap(pending, error);
                return;
            }
        };
        let (ready, record_len) = match pending
            .capture
            .as_mut()
            .ok_or(crate::native_state::NativeStateError::InvalidState)
            .and_then(|capture| capture.step(&mut pending.scratch[..step_bytes]))
        {
            Ok(event) => (
                matches!(
                    event.kind,
                    crate::native_state::NativeCheckpointChunkKind::Ready
                ),
                event.bytes.len(),
            ),
            Err(error) => {
                self.fail_native_bootstrap(pending, error);
                return;
            }
        };
        let fragments = record_len.div_ceil(pending.chunk_bytes);
        let Some(chunk_count) = pending.chunk_count.checked_add(fragments) else {
            self.fail_native_bootstrap(
                pending,
                crate::native_state::NativeStateError::LimitExceeded,
            );
            return;
        };
        if chunk_count > pending.max_chunks {
            self.fail_native_bootstrap(
                pending,
                crate::native_state::NativeStateError::LimitExceeded,
            );
            return;
        }
        let mut payload = match reserve_native_bytes(record_len) {
            Ok(payload) => payload,
            Err(error) => {
                self.fail_native_bootstrap(pending, error);
                return;
            }
        };
        payload.extend_from_slice(&pending.scratch[..record_len]);
        let Some(retained_bytes) = pending
            .retained_bytes
            .checked_add(payload.capacity())
            .filter(|bytes| *bytes <= pending.capture_bytes)
        else {
            self.fail_native_bootstrap(
                pending,
                crate::native_state::NativeStateError::LimitExceeded,
            );
            return;
        };
        pending.records.push(Bytes::from(payload));
        pending.retained_bytes = retained_bytes;
        pending.chunk_count = chunk_count;
        if !ready {
            self.pending_native_bootstrap = Some(pending);
            return;
        }

        let completed = {
            let Some(capture) = pending.capture.take() else {
                self.fail_native_bootstrap(
                    pending,
                    crate::native_state::NativeStateError::InvalidState,
                );
                return;
            };
            let mut host = self.terminal.borrow_mut();
            match &mut *host {
                CanonicalTerminal::Native(manager) => manager.finish_generation_capture(capture),
                CanonicalTerminal::Plain(_) => {
                    Err(crate::native_state::NativeStateError::InvalidState)
                }
            }
        };
        let (cursor, seed) = match completed {
            Ok(completed) => completed,
            Err(error) => {
                self.apply_native_replay(&pending.replay);
                for waiter in pending.waiters {
                    let _ = waiter.reply.send(Err(error));
                }
                self.start_next_native_bootstrap();
                return;
            }
        };
        self.apply_native_replay(&pending.replay);
        self.finish_native_bootstrap(pending, cursor, seed);
        self.start_next_native_bootstrap();
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn native_bootstrap_reply(
        &self,
        req: &NativeBootstrapRequest,
        records: &[Bytes],
        record_retained_bytes: usize,
        cursor: crate::native_state::OpaqueHistoryCursor,
        base_seq: u64,
    ) -> Result<NativeBootstrapReply, crate::native_state::NativeStateError> {
        let mut frames = Vec::new();
        frames
            .try_reserve(records.len().saturating_add(2))
            .map_err(|_| crate::native_state::NativeStateError::OutOfMemory)?;
        frames.push(FrameKind::BootstrapBegin {
            terminal_id: req.terminal_id.clone(),
            stream_id: req.stream_id,
            bootstrap_id: req.bootstrap_id,
            profile: phux_protocol::caps::BootstrapStreamProfile::NativeState {
                codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2,
            },
            cols: self.cols,
            rows: self.rows,
            base_seq,
        });
        let chunk_bytes = usize::try_from(req.limits.max_chunk_bytes())
            .map_err(|_| crate::native_state::NativeStateError::LimitExceeded)?;
        let mut chunk_seq = 0_u32;
        for record in records {
            for fragment in record.chunks(chunk_bytes) {
                if frames.len() >= req.max_frames.saturating_sub(1) {
                    return Err(crate::native_state::NativeStateError::LimitExceeded);
                }
                frames.push(FrameKind::BootstrapChunk {
                    terminal_id: req.terminal_id.clone(),
                    stream_id: req.stream_id,
                    bootstrap_id: req.bootstrap_id,
                    chunk_seq,
                    payload: record.slice_ref(fragment),
                });
                chunk_seq = chunk_seq
                    .checked_add(1)
                    .ok_or(crate::native_state::NativeStateError::LimitExceeded)?;
            }
        }
        let retained_bytes = record_retained_bytes
            .checked_add(cursor.len())
            .filter(|bytes| *bytes <= req.max_bytes)
            .ok_or(crate::native_state::NativeStateError::LimitExceeded)?;
        frames.push(FrameKind::BootstrapReady {
            terminal_id: req.terminal_id.clone(),
            stream_id: req.stream_id,
            bootstrap_id: req.bootstrap_id,
            history_cursor: Some(Bytes::copy_from_slice(&cursor)),
        });
        Ok(NativeBootstrapReply {
            frames,
            retained_bytes,
            base_seq,
            publication_cursor: cursor,
        })
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    #[allow(
        clippy::too_many_lines,
        reason = "the READY publication fence: install the generation, charge its reservation, notify every waiting owner, and unwind all of it on any failure. The unwind must see every resource the happy path acquired, so the two halves cannot be separated without duplicating the cleanup"
    )]
    pub(super) fn finish_native_bootstrap(
        &mut self,
        pending: PendingNativeBootstrap,
        cursor: crate::native_state::OpaqueHistoryCursor,
        seed: crate::native_state::NativeGenerationSeed,
    ) {
        let record_retained_bytes = pending.retained_bytes;
        let mut prepared = Vec::new();
        for waiter in pending.waiters {
            match self.native_bootstrap_reply(
                &waiter,
                &pending.records,
                record_retained_bytes,
                cursor,
                pending.base_seq,
            ) {
                Ok(reply) => prepared.push((waiter, reply)),
                Err(error) => {
                    let _ = waiter.reply.send(Err(error));
                }
            }
        }
        if prepared.is_empty() {
            return;
        }
        let installed_new = {
            let mut host = self.terminal.borrow_mut();
            let manager = match &mut *host {
                CanonicalTerminal::Native(manager) => manager,
                CanonicalTerminal::Plain(_) => {
                    for (waiter, _) in prepared {
                        let _ = waiter
                            .reply
                            .send(Err(crate::native_state::NativeStateError::InvalidState));
                    }
                    return;
                }
            };
            if manager.has_generation(&cursor) {
                drop(seed);
                Ok(false)
            } else {
                let bounds = seed.bounds();
                bounds
                    .required_reserved_bytes()
                    .and_then(|reserved| manager.install_generation(cursor, seed, bounds, reserved))
                    .map(|()| true)
            }
        };
        let Ok(installed_new) = installed_new else {
            for (waiter, _) in prepared {
                let _ = waiter
                    .reply
                    .send(Err(crate::native_state::NativeStateError::LimitExceeded));
            }
            return;
        };
        if let Some(existing) = self.native_publications.get(&cursor)
            && (existing.base_seq != pending.base_seq || existing.replay != pending.replay)
        {
            for (waiter, _) in prepared {
                let _ = waiter
                    .reply
                    .send(Err(crate::native_state::NativeStateError::LimitExceeded));
            }
            return;
        }

        let mut waiting = HashSet::new();
        let mut installed = 0_usize;
        for (waiter, reply_value) in prepared {
            let retained = {
                let mut host = self.terminal.borrow_mut();
                match &mut *host {
                    CanonicalTerminal::Native(manager) => {
                        if installed_new && installed == 0 {
                            true
                        } else {
                            manager.retain_generation(&cursor).is_ok()
                        }
                    }
                    CanonicalTerminal::Plain(_) => false,
                }
            };
            if !retained {
                let _ = waiter
                    .reply
                    .send(Err(crate::native_state::NativeStateError::LimitExceeded));
                continue;
            }
            installed += 1;
            self.native_cursor_owners.insert(
                waiter.owner,
                NativeCursorOwner {
                    cursor,
                    record_index: 0,
                    touched: tokio::time::Instant::now(),
                    next_page_seq: 1,
                    terminal_id: waiter.terminal_id,
                    stream_id: waiter.stream_id,
                    bootstrap_id: waiter.bootstrap_id,
                },
            );
            waiting.insert(waiter.owner);
            if waiter.reply.send(Ok(reply_value)).is_err() {
                waiting.remove(&waiter.owner);
                self.release_native_owner(waiter.owner);
            }
        }
        if !waiting.is_empty() {
            use std::collections::hash_map::Entry;
            match self.native_publications.entry(cursor) {
                Entry::Occupied(mut entry) => {
                    entry.get_mut().waiting.extend(waiting);
                }
                Entry::Vacant(entry) => {
                    entry.insert(NativePublicationGeneration {
                        base_seq: pending.base_seq,
                        replay: pending.replay,
                        replay_bytes: pending.replay_bytes,
                        waiting,
                    });
                }
            }
        }
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn fail_native_bootstrap(
        &mut self,
        mut pending: PendingNativeBootstrap,
        error: crate::native_state::NativeStateError,
    ) {
        if let Some(capture) = pending.capture.take()
            && let CanonicalTerminal::Native(manager) = &mut *self.terminal.borrow_mut()
        {
            manager.abort_generation_capture(capture);
        }
        self.apply_native_replay(&pending.replay);
        for waiter in pending.waiters {
            let error = match error {
                crate::native_state::NativeStateError::OutOfMemory => {
                    crate::native_state::NativeStateError::OutOfMemory
                }
                _ => crate::native_state::NativeStateError::LimitExceeded,
            };
            let _ = waiter.reply.send(Err(error));
        }
        self.start_next_native_bootstrap();
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn start_next_native_bootstrap(&mut self) {
        if self.pending_native_bootstrap.is_none()
            && let Some(next) = self.native_bootstrap_backlog.pop_front()
        {
            self.start_native_bootstrap(next);
        }
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn apply_native_replay(&mut self, replay: &VecDeque<(u64, Bytes)>) {
        if replay.is_empty() {
            return;
        }
        for (_, bytes) in replay {
            self.terminal.borrow_mut().vt_write(bytes);
            self.answer_color_queries(bytes);
            self.source_events_from_chunk(bytes);
        }
        self.publish_input_snapshot();
        self.terminal_dirty_since_tick = true;
        self.agent_dirty_since_detect = true;
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn buffer_native_live_output(&mut self, seq: u64, bytes: &Bytes) -> bool {
        let mut deferred = false;
        let overflow = self
            .pending_native_bootstrap
            .as_ref()
            .is_some_and(|pending| {
                pending
                    .replay_bytes
                    .checked_add(bytes.len())
                    .is_none_or(|total| total > MAX_NATIVE_REPLAY_BYTES)
            });
        if overflow {
            if let Some(mut pending) = self.pending_native_bootstrap.take() {
                if let Some(capture) = pending.capture.take()
                    && let CanonicalTerminal::Native(manager) = &mut *self.terminal.borrow_mut()
                {
                    manager.abort_generation_capture(capture);
                }
                self.apply_native_replay(&pending.replay);
                for waiter in pending.waiters {
                    let _ = waiter
                        .reply
                        .send(Err(crate::native_state::NativeStateError::LimitExceeded));
                }
            }
        } else if let Some(pending) = self.pending_native_bootstrap.as_mut() {
            pending.replay_bytes += bytes.len();
            pending.replay.push_back((seq, bytes.clone()));
            deferred = true;
        }

        let mut overflowed = Vec::new();
        for (cursor, publication) in &mut self.native_publications {
            match publication.replay_bytes.checked_add(bytes.len()) {
                Some(total) if total <= MAX_NATIVE_REPLAY_BYTES => {
                    publication.replay_bytes = total;
                    publication.replay.push_back((seq, bytes.clone()));
                }
                _ => overflowed.push(*cursor),
            }
        }
        for cursor in overflowed {
            if let Some(publication) = self.native_publications.remove(&cursor) {
                for owner in publication.waiting {
                    self.release_native_owner(owner);
                }
            }
        }
        deferred
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn handle_native_publication(&mut self, req: NativePublicationRequest) {
        let valid = self
            .native_cursor_owners
            .get(&req.owner)
            .is_some_and(|binding| {
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
        if !publication.waiting.remove(&req.owner) {
            let _ = req
                .reply
                .send(Err(crate::native_state::NativeStateError::InvalidHandle));
            return;
        }
        let replay = publication.replay.iter().cloned().collect();
        let live = self.output_tx.subscribe();
        let remove = publication.waiting.is_empty();
        if remove {
            self.native_publications.remove(&req.cursor);
        }
        let _ = req.reply.send(Ok(NativePublicationReply { replay, live }));
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    #[allow(
        clippy::too_many_lines,
        reason = "one HISTORY request end to end: validate the cursor, size the caller buffer against the generation bounds, pull or serve-from-cache, and answer. Every early return needs the permit released and the request answered exactly once, which is what keeps this a single function"
    )]
    pub(super) fn handle_native_history(&mut self, req: NativeHistoryRequest) {
        let NativeHistoryRequest {
            permit,
            owner,
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor: wire_cursor,
            max_bytes,
            max_rows,
            limits,
            reply,
        } = req;
        // A cursor the actor cannot honour is a routine race, not a fault: a
        // resize drains every binding (`invalidate_all_native_cursors`) while
        // the client's HISTORY_REQUEST for the generation it was just handed
        // is still in flight. That is guaranteed to happen for a pane created
        // mid-attach, which the layout resizes immediately after its bootstrap
        // (phux-rv52). HISTORY_TOMBSTONE is the frame the protocol defines for
        // exactly this -- it degrades the one replica's scrollback and leaves
        // the attach intact -- so an unusable cursor is answered, never
        // escalated to a connection-scoped Error.
        let stale = |wire_cursor: Bytes| FrameKind::HistoryTombstone {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: wire_cursor,
            reason: phux_protocol::wire::frame::HistoryTombstoneReason::Stale,
        };
        let Ok(cursor): Result<crate::native_state::OpaqueHistoryCursor, _> =
            wire_cursor.as_ref().try_into()
        else {
            // Unlike the races below this one is a client protocol violation,
            // so it is worth a log line -- but not worth ending the attach.
            warn!(
                len = wire_cursor.len(),
                ?terminal_id,
                "HISTORY_REQUEST carried a malformed cursor"
            );
            let _ = reply.send(NativeHistoryReply {
                permit,
                result: Ok(stale(wire_cursor)),
            });
            return;
        };
        let Some(binding) = self.native_cursor_owners.get(&owner) else {
            let _ = reply.send(NativeHistoryReply {
                permit,
                result: Ok(stale(wire_cursor)),
            });
            return;
        };
        if binding.cursor != cursor
            || binding.terminal_id != terminal_id
            || binding.stream_id != stream_id
            || binding.bootstrap_id != bootstrap_id
        {
            let _ = reply.send(NativeHistoryReply {
                permit,
                result: Ok(stale(wire_cursor)),
            });
            return;
        }
        let page_seq = binding.next_page_seq;
        let record_index = binding.record_index;
        let bound = max_bytes.min(limits.max_history_page_bytes());
        if bound == 0 || max_rows == 0 {
            let _ = reply.send(NativeHistoryReply {
                permit,
                result: Ok(FrameKind::HistoryRejected {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    cursor: wire_cursor,
                    reason: phux_protocol::wire::frame::HistoryRejectionReason::ZeroLimit,
                    required_bytes: 1,
                    required_rows: 1,
                }),
            });
            return;
        }
        let delivery = {
            let mut host = self.terminal.borrow_mut();
            match host.native_manager() {
                Ok(manager) => manager.history_record_at(&cursor, record_index, bound, max_rows),
                Err(error) => Err(error),
            }
        };
        let (result, keep) = match delivery {
            Ok(record) => {
                let Some(next_page_seq) = page_seq.checked_add(1) else {
                    self.release_native_owner(owner);
                    let _ = reply.send(NativeHistoryReply {
                        permit,
                        result: Err(crate::native_state::NativeStateError::LimitExceeded),
                    });
                    return;
                };
                let Ok(rows) = u32::try_from(record.rows) else {
                    self.release_native_owner(owner);
                    let _ = reply.send(NativeHistoryReply {
                        permit,
                        result: Err(crate::native_state::NativeStateError::LimitExceeded),
                    });
                    return;
                };
                if record.finish {
                    self.release_native_owner(owner);
                } else if let Some(binding) = self.native_cursor_owners.get_mut(&owner) {
                    binding.record_index += 1;
                    binding.next_page_seq = next_page_seq;
                    binding.touched = tokio::time::Instant::now();
                }
                (
                    Ok(FrameKind::HistoryPage {
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        page_seq,
                        cursor: wire_cursor.clone(),
                        next_cursor: (!record.finish).then_some(wire_cursor),
                        payload: record.bytes,
                        rows,
                    }),
                    !record.finish,
                )
            }
            Err(crate::native_state::NativeStateError::OutOfSpace {
                required_bytes,
                required_rows,
            }) => {
                let frame = match (
                    u32::try_from(required_bytes),
                    u32::try_from(required_rows.max(1)),
                ) {
                    (Ok(required_bytes), Ok(required_rows))
                        if required_bytes != 0
                            && required_bytes <= limits.max_history_page_bytes()
                            && required_rows <= phux_protocol::MAX_HISTORY_PAGE_ROWS =>
                    {
                        FrameKind::HistoryRejected {
                            terminal_id,
                            stream_id,
                            bootstrap_id,
                            cursor: wire_cursor,
                            reason: phux_protocol::wire::frame::HistoryRejectionReason::TooSmall,
                            required_bytes,
                            required_rows,
                        }
                    }
                    _ => {
                        self.release_native_owner(owner);
                        FrameKind::HistoryTombstone {
                            terminal_id,
                            stream_id,
                            bootstrap_id,
                            cursor: wire_cursor,
                            reason: phux_protocol::wire::frame::HistoryTombstoneReason::Limit,
                        }
                    }
                };
                (Ok(frame), self.native_cursor_owners.contains_key(&owner))
            }
            Err(crate::native_state::NativeStateError::ImportBusy) => (
                Ok(FrameKind::HistoryRejected {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    cursor: wire_cursor,
                    reason: phux_protocol::wire::frame::HistoryRejectionReason::Busy,
                    required_bytes: bound,
                    required_rows: max_rows,
                }),
                true,
            ),
            Err(error) => {
                let reason = match error {
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
                };
                self.release_native_owner(owner);
                (
                    Ok(FrameKind::HistoryTombstone {
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        cursor: wire_cursor,
                        reason,
                    }),
                    false,
                )
            }
        };
        if reply.send(NativeHistoryReply { permit, result }).is_err() && keep {
            self.release_native_owner(owner);
        }
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn publish_native_control(&self, owner: u64, frame: FrameKind) {
        let _ = self.output_tx.send(PaneOutput::Control { owner, frame });
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn invalidate_all_native_cursors(
        &mut self,
        reason: phux_protocol::wire::frame::TombstoneReason,
    ) {
        self.native_bootstrap_backlog.clear();
        if let Some(pending) = self.pending_native_bootstrap.take() {
            self.fail_native_bootstrap(pending, crate::native_state::NativeStateError::Resize);
        }
        let bindings: Vec<_> = self.native_cursor_owners.drain().collect();
        self.native_publications.clear();
        let last_valid_seq = self.raw_seq;
        for (owner, binding) in bindings {
            self.publish_native_control(
                owner,
                FrameKind::BootstrapTombstone {
                    terminal_id: binding.terminal_id,
                    stream_id: binding.stream_id,
                    bootstrap_id: binding.bootstrap_id,
                    reason,
                    last_valid_seq,
                },
            );
            if let CanonicalTerminal::Native(manager) = &mut *self.terminal.borrow_mut() {
                let _ = manager.release_generation(&binding.cursor);
            }
        }
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn invalidate_native_owner(
        &mut self,
        owner: u64,
        reason: phux_protocol::wire::frame::TombstoneReason,
    ) {
        if let Some(pending) = self.pending_native_bootstrap.as_mut() {
            pending.waiters.retain(|waiter| waiter.owner != owner);
        }
        self.native_bootstrap_backlog
            .retain(|waiter| waiter.owner != owner);
        let Some(binding) = self.native_cursor_owners.remove(&owner) else {
            return;
        };
        if let Some(publication) = self.native_publications.get_mut(&binding.cursor) {
            publication.waiting.remove(&owner);
            if publication.waiting.is_empty() {
                self.native_publications.remove(&binding.cursor);
            }
        }
        self.publish_native_control(
            owner,
            FrameKind::BootstrapTombstone {
                terminal_id: binding.terminal_id,
                stream_id: binding.stream_id,
                bootstrap_id: binding.bootstrap_id,
                reason,
                last_valid_seq: self.raw_seq,
            },
        );
        if let CanonicalTerminal::Native(manager) = &mut *self.terminal.borrow_mut() {
            let _ = manager.release_generation(&binding.cursor);
        }
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn release_native_owner(&mut self, owner: u64) {
        if let Some(pending) = self.pending_native_bootstrap.as_mut() {
            pending.waiters.retain(|waiter| waiter.owner != owner);
        }
        self.native_bootstrap_backlog
            .retain(|waiter| waiter.owner != owner);
        let Some(binding) = self.native_cursor_owners.remove(&owner) else {
            return;
        };
        if let Some(publication) = self.native_publications.get_mut(&binding.cursor) {
            publication.waiting.remove(&owner);
            if publication.waiting.is_empty() {
                self.native_publications.remove(&binding.cursor);
            }
        }
        if let CanonicalTerminal::Native(manager) = &mut *self.terminal.borrow_mut() {
            let _ = manager.release_generation(&binding.cursor);
        }
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn expire_native_cursors(&mut self) {
        let cutoff = tokio::time::Instant::now() - NATIVE_HISTORY_TTL;
        let owners: Vec<_> = self
            .native_cursor_owners
            .iter()
            .filter_map(|(owner, binding)| (binding.touched <= cutoff).then_some(*owner))
            .collect();
        for owner in owners {
            let Some(binding) = self.native_cursor_owners.get(&owner) else {
                continue;
            };
            self.publish_native_control(
                owner,
                FrameKind::HistoryTombstone {
                    terminal_id: binding.terminal_id.clone(),
                    stream_id: binding.stream_id,
                    bootstrap_id: binding.bootstrap_id,
                    cursor: Bytes::copy_from_slice(&binding.cursor),
                    reason: phux_protocol::wire::frame::HistoryTombstoneReason::Expired,
                },
            );
            self.release_native_owner(owner);
        }
    }

    pub(super) fn handle_native_actor_request(&mut self, req: NativeActorRequest) {
        match req {
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            NativeActorRequest::Bootstrap(req) => self.handle_native_bootstrap(req),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            NativeActorRequest::Publication(req) => self.handle_native_publication(req),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            NativeActorRequest::History(req) => self.handle_native_history(req),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            NativeActorRequest::Release(req) => self.release_native_owner(req.owner),
            #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
            NativeActorRequest::Disabled => unreachable!("disabled native receiver"),
        }
    }

    pub(super) const fn native_bootstrap_pending(&self) -> bool {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        {
            self.pending_native_bootstrap.is_some()
        }
        #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
        {
            false
        }
    }

    pub(super) fn cooperative_native_step(&mut self) {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        self.step_native_bootstrap();
    }
}
