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

/// Starting width of the per-pane native checkpoint scratch buffer.
///
/// One standard libghostty page is the unit a prefix record is built from, so
/// a real active-area record is hundreds of kilobytes at most. The buffer
/// grows to the exact `required_bytes` the engine reports when a record does
/// not fit, so this is a starting point, never a cap.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(super) const INITIAL_NATIVE_SCRATCH_BYTES: usize = 64 * 1024;

/// Width of the scratch buffer a fresh bootstrap starts with.
///
/// The negotiated `ceiling` is the whole connection staging budget (64 MiB by
/// default) and the engine advertises `u32::MAX` for its own record bound, so
/// sizing the buffer from either committed two orders of magnitude more memory
/// than any real record uses. Seed one page-sized window instead and let the
/// engine's exact `OutOfSpace { required_bytes }` widen it.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(super) const fn initial_native_scratch_bytes(ceiling: usize) -> usize {
    if ceiling < INITIAL_NATIVE_SCRATCH_BYTES {
        ceiling
    } else {
        INITIAL_NATIVE_SCRATCH_BYTES
    }
}

/// Widen `scratch` to exactly `required_bytes` without ever aborting on a
/// failed allocation.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
fn grow_native_scratch(
    scratch: &mut Vec<u8>,
    required_bytes: usize,
) -> Result<(), crate::native_state::NativeStateError> {
    let mut grown = reserve_native_bytes(required_bytes)?;
    grown.resize(required_bytes, 0);
    *scratch = grown;
    Ok(())
}

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
        // The scratch buffer holds ONE opaque prefix record at a time, not the
        // whole connection staging budget. The engine advertises `u32::MAX`
        // for `max_record_bytes`, so sizing it from the negotiated ceiling
        // committed (and zeroed) 64 MiB per pane per attach — two orders of
        // magnitude above the ~760 KiB a real active-area record needs. Start
        // at one page-sized window and let `step_native_bootstrap` grow it to
        // the exact `required_bytes` libghostty reports; the ceiling still
        // bounds it.
        let scratch_ceiling = match native_step_bytes(capture_bytes, 0, capture.max_record_bytes())
        {
            Ok(bytes) => bytes,
            Err(error) => {
                if let CanonicalTerminal::Native(manager) = &mut *self.terminal.borrow_mut() {
                    manager.abort_generation_capture(capture);
                }
                let _ = req.reply.send(Err(error));
                return;
            }
        };
        let scratch_bytes = initial_native_scratch_bytes(scratch_ceiling);
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
            base_seq: self.core.seq(),
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
        let step_ceiling = match native_step_bytes(
            pending.capture_bytes,
            pending.retained_bytes,
            pending.capture.as_ref().map_or(
                0,
                crate::native_state::NativeManagedCapture::max_record_bytes,
            ),
        ) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.fail_native_bootstrap(pending, error);
                return;
            }
        };
        let scratch_len = pending.scratch.len();
        let step_bytes = step_ceiling.min(scratch_len);
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
            // The engine reports the exact width of the record it could not
            // write. Grow the scratch to it, still under the negotiated
            // ceiling, and retry on the next cooperative turn: `next` does not
            // consume the record when it returns `OutOfSpace`.
            Err(crate::native_state::NativeStateError::OutOfSpace {
                required_bytes,
                required_rows: 0,
            }) if required_bytes > scratch_len && required_bytes <= step_ceiling => {
                match grow_native_scratch(&mut pending.scratch, required_bytes) {
                    Ok(()) => self.pending_native_bootstrap = Some(pending),
                    Err(error) => self.fail_native_bootstrap(pending, error),
                }
                return;
            }
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

    /// The READY publication fence: install the generation, charge its
    /// reservation, notify every waiting owner, and unwind all of it on any
    /// failure. Each stage below either answers every prepared waiter and
    /// stops, or hands the still-live set to the next one, so a waiter is
    /// answered exactly once no matter where the fence fails.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn finish_native_bootstrap(
        &mut self,
        pending: PendingNativeBootstrap,
        cursor: crate::native_state::OpaqueHistoryCursor,
        seed: crate::native_state::NativeGenerationSeed,
    ) {
        let PendingNativeBootstrap {
            waiters,
            records,
            retained_bytes,
            base_seq,
            replay,
            replay_bytes,
            ..
        } = pending;
        let prepared = self.prepare_native_bootstrap_replies(
            waiters,
            &records,
            retained_bytes,
            cursor,
            base_seq,
        );
        if prepared.is_empty() {
            return;
        }
        let installed_new = match self.install_native_generation(cursor, seed) {
            Ok(installed_new) => installed_new,
            Err(error) => {
                fail_native_waiters(prepared, error);
                return;
            }
        };
        if self.native_publication_conflicts(cursor, base_seq, &replay) {
            fail_native_waiters(
                prepared,
                crate::native_state::NativeStateError::LimitExceeded,
            );
            return;
        }
        let waiting = self.register_native_bootstrap_waiters(prepared, cursor, installed_new);
        if waiting.is_empty() {
            return;
        }
        self.record_native_publication(cursor, base_seq, replay, replay_bytes, waiting);
    }

    /// Build every waiter's BOOTSTRAP reply up front, answering (and dropping)
    /// the ones whose reply cannot be assembled within their own limits.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn prepare_native_bootstrap_replies(
        &self,
        waiters: Vec<NativeBootstrapRequest>,
        records: &[Bytes],
        record_retained_bytes: usize,
        cursor: crate::native_state::OpaqueHistoryCursor,
        base_seq: u64,
    ) -> Vec<(NativeBootstrapRequest, NativeBootstrapReply)> {
        let mut prepared = Vec::new();
        for waiter in waiters {
            match self.native_bootstrap_reply(
                &waiter,
                records,
                record_retained_bytes,
                cursor,
                base_seq,
            ) {
                Ok(reply) => prepared.push((waiter, reply)),
                Err(error) => {
                    let _ = waiter.reply.send(Err(error));
                }
            }
        }
        prepared
    }

    /// Install `cursor`'s generation on the native manager, charging its
    /// reservation. `Ok(true)` when this call installed it, `Ok(false)` when
    /// the manager already held it (the seed is then dropped unused).
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn install_native_generation(
        &self,
        cursor: crate::native_state::OpaqueHistoryCursor,
        seed: crate::native_state::NativeGenerationSeed,
    ) -> Result<bool, crate::native_state::NativeStateError> {
        let mut host = self.terminal.borrow_mut();
        let manager = match &mut *host {
            CanonicalTerminal::Native(manager) => manager,
            CanonicalTerminal::Plain(_) => {
                return Err(crate::native_state::NativeStateError::InvalidState);
            }
        };
        if manager.has_generation(&cursor) {
            drop(seed);
            return Ok(false);
        }
        let bounds = seed.bounds();
        bounds
            .required_reserved_bytes()
            .and_then(|reserved| manager.install_generation(cursor, seed, bounds, reserved))
            .map(|()| true)
            .map_err(|_| crate::native_state::NativeStateError::LimitExceeded)
    }

    /// Whether an already-published generation for `cursor` disagrees with the
    /// capture we are about to publish. Republishing over it would hand two
    /// replicas different bytes for the same cursor.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn native_publication_conflicts(
        &self,
        cursor: crate::native_state::OpaqueHistoryCursor,
        base_seq: u64,
        replay: &VecDeque<(u64, Bytes)>,
    ) -> bool {
        self.native_publications
            .get(&cursor)
            .is_some_and(|existing| existing.base_seq != base_seq || existing.replay != *replay)
    }

    /// Retain the installed generation once per waiter, bind each owner to the
    /// cursor, and ship its reply. Returns the owners still waiting on the
    /// publication (a waiter whose reply channel is gone is released again).
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn register_native_bootstrap_waiters(
        &mut self,
        prepared: Vec<(NativeBootstrapRequest, NativeBootstrapReply)>,
        cursor: crate::native_state::OpaqueHistoryCursor,
        installed_new: bool,
    ) -> HashSet<u64> {
        let mut waiting = HashSet::new();
        let mut installed = 0_usize;
        for (waiter, reply_value) in prepared {
            if !self.retain_native_generation(&cursor, installed_new && installed == 0) {
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
        waiting
    }

    /// Charge one waiter's retain against the generation. `rides_install` is
    /// the first waiter on a freshly installed generation: the install itself
    /// already retained it, so it takes no second retain.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn retain_native_generation(
        &self,
        cursor: &crate::native_state::OpaqueHistoryCursor,
        rides_install: bool,
    ) -> bool {
        let mut host = self.terminal.borrow_mut();
        match &mut *host {
            CanonicalTerminal::Native(manager) => {
                rides_install || manager.retain_generation(cursor).is_ok()
            }
            CanonicalTerminal::Plain(_) => false,
        }
    }

    /// Record (or extend) the publication generation the waiting owners are
    /// parked on, so a later PUBLICATION request can serve them the replay.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn record_native_publication(
        &mut self,
        cursor: crate::native_state::OpaqueHistoryCursor,
        base_seq: u64,
        replay: VecDeque<(u64, Bytes)>,
        replay_bytes: usize,
        waiting: HashSet<u64>,
    ) {
        use std::collections::hash_map::Entry;
        match self.native_publications.entry(cursor) {
            Entry::Occupied(mut entry) => {
                entry.get_mut().waiting.extend(waiting);
            }
            Entry::Vacant(entry) => {
                entry.insert(NativePublicationGeneration {
                    base_seq,
                    replay,
                    replay_bytes,
                    waiting,
                });
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
        let live = self.core.output_tx.subscribe();
        let remove = publication.waiting.is_empty();
        if remove {
            self.native_publications.remove(&req.cursor);
        }
        let _ = req.reply.send(Ok(NativePublicationReply { replay, live }));
    }

    /// Serve one HISTORY request end to end: validate the cursor, size the
    /// caller buffer against the generation bounds, pull or serve-from-cache,
    /// and answer.
    ///
    /// Every exit path releases the permit and answers the request exactly
    /// once, which is what the staged early returns below preserve.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
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
        let id = HistoryFrameId {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor: wire_cursor,
        };
        // A cursor the actor cannot honour is a routine race, not a fault: a
        // resize drains every binding (`invalidate_all_native_cursors`) while
        // the client's HISTORY_REQUEST for the generation it was just handed
        // is still in flight. That is guaranteed to happen for a pane created
        // mid-attach, which the layout resizes immediately after its bootstrap
        // (phux-rv52). HISTORY_TOMBSTONE is the frame the protocol defines for
        // exactly this -- it degrades the one replica's scrollback and leaves
        // the attach intact -- so an unusable cursor is answered, never
        // escalated to a connection-scoped Error.
        let stale = phux_protocol::wire::frame::HistoryTombstoneReason::Stale;
        let Ok(cursor): Result<crate::native_state::OpaqueHistoryCursor, _> =
            id.cursor.as_ref().try_into()
        else {
            // Unlike the races below this one is a client protocol violation,
            // so it is worth a log line -- but not worth ending the attach.
            warn!(
                len = id.cursor.len(),
                terminal_id = ?id.terminal_id,
                "HISTORY_REQUEST carried a malformed cursor"
            );
            answer_history(reply, permit, Ok(id.tombstone(stale)));
            return;
        };
        let Some((page_seq, record_index)) = self.history_binding(owner, &id, cursor) else {
            answer_history(reply, permit, Ok(id.tombstone(stale)));
            return;
        };
        let bound = max_bytes.min(limits.max_history_page_bytes());
        if bound == 0 || max_rows == 0 {
            let frame = id.rejected(
                phux_protocol::wire::frame::HistoryRejectionReason::ZeroLimit,
                1,
                1,
            );
            answer_history(reply, permit, Ok(frame));
            return;
        }
        let delivery = self.history_record_at(&cursor, record_index, bound, max_rows);
        let (result, keep) = match delivery {
            Ok(record) => {
                let finish = record.finish;
                match self.advance_history_binding(owner, &record, page_seq) {
                    Ok(rows) => (Ok(id.page(page_seq, record.bytes, rows, finish)), !finish),
                    Err(error) => {
                        answer_history(reply, permit, Err(error));
                        return;
                    }
                }
            }
            Err(crate::native_state::NativeStateError::OutOfSpace {
                required_bytes,
                required_rows,
            }) => {
                let (frame, keep) =
                    self.history_out_of_space(owner, id, limits, required_bytes, required_rows);
                (Ok(frame), keep)
            }
            Err(crate::native_state::NativeStateError::ImportBusy) => (
                Ok(id.rejected(
                    phux_protocol::wire::frame::HistoryRejectionReason::Busy,
                    bound,
                    max_rows,
                )),
                true,
            ),
            Err(error) => {
                let reason = history_tombstone_reason(error);
                self.release_native_owner(owner);
                (Ok(id.tombstone(reason)), false)
            }
        };
        if reply.send(NativeHistoryReply { permit, result }).is_err() && keep {
            self.release_native_owner(owner);
        }
    }

    /// Resolve `owner`'s cursor binding, returning its next page sequence and
    /// record index. `None` when the owner has no binding or the binding does
    /// not match this request's generation identity -- both routine races the
    /// caller answers with a stale tombstone.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn history_binding(
        &self,
        owner: u64,
        id: &HistoryFrameId,
        cursor: crate::native_state::OpaqueHistoryCursor,
    ) -> Option<(u64, usize)> {
        let binding = self.native_cursor_owners.get(&owner)?;
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
    fn history_record_at(
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

    /// Advance `owner`'s binding past the record just served, returning the
    /// page's row count. A binding that can no longer be advanced (sequence or
    /// row count out of range) releases the owner and fails the request; a
    /// finished record releases the owner too, since there is no next page.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn advance_history_binding(
        &mut self,
        owner: u64,
        record: &crate::native_state::CachedNativeHistoryRecord,
        page_seq: u64,
    ) -> Result<u32, crate::native_state::NativeStateError> {
        let Some(next_page_seq) = page_seq.checked_add(1) else {
            self.release_native_owner(owner);
            return Err(crate::native_state::NativeStateError::LimitExceeded);
        };
        let Ok(rows) = u32::try_from(record.rows) else {
            self.release_native_owner(owner);
            return Err(crate::native_state::NativeStateError::LimitExceeded);
        };
        if record.finish {
            self.release_native_owner(owner);
        } else if let Some(binding) = self.native_cursor_owners.get_mut(&owner) {
            binding.record_index += 1;
            binding.next_page_seq = next_page_seq;
            binding.touched = tokio::time::Instant::now();
        }
        Ok(rows)
    }

    /// Answer a record the caller's buffer cannot hold: `HISTORY_REJECTED` with
    /// the exact requirement when the client can retry within the negotiated
    /// bounds, else a `HISTORY_TOMBSTONE` that also releases the owner. Returns
    /// the frame and whether the owner is still bound.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn history_out_of_space(
        &mut self,
        owner: u64,
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
                self.release_native_owner(owner);
                id.tombstone(phux_protocol::wire::frame::HistoryTombstoneReason::Limit)
            }
        };
        (frame, self.native_cursor_owners.contains_key(&owner))
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) fn publish_native_control(&self, owner: u64, frame: FrameKind) {
        let _ = self
            .core
            .output_tx
            .send(PaneOutput::Control { owner, frame });
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    ///
    /// Returns the pumps it tombstoned, so a reflow that schedules no
    /// everyone-resync can still address one to them (phux-p5bo).
    pub(super) fn invalidate_all_native_cursors(
        &mut self,
        reason: phux_protocol::wire::frame::TombstoneReason,
    ) -> Vec<crate::resource::ResyncTarget> {
        self.native_bootstrap_backlog.clear();
        if let Some(pending) = self.pending_native_bootstrap.take() {
            self.fail_native_bootstrap(pending, crate::native_state::NativeStateError::Resize);
        }
        let bindings: Vec<_> = self.native_cursor_owners.drain().collect();
        self.native_publications.clear();
        let last_valid_seq = self.core.seq();
        let mut tombstoned = Vec::with_capacity(bindings.len());
        for (owner, binding) in bindings {
            tombstoned.push(crate::resource::ResyncTarget {
                owner,
                stream_id: binding.stream_id,
                bootstrap_id: binding.bootstrap_id,
            });
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
        tombstoned
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
                last_valid_seq: self.core.seq(),
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

/// Answer every still-live prepared waiter with `error`.
///
/// The unwind path of the READY publication fence: whichever stage fails, the
/// waiters it was holding are answered exactly once and dropped.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
fn fail_native_waiters(
    prepared: Vec<(NativeBootstrapRequest, NativeBootstrapReply)>,
    error: crate::native_state::NativeStateError,
) {
    for (waiter, _) in prepared {
        let _ = waiter.reply.send(Err(error));
    }
}

/// The wire identity every HISTORY answer frame is stamped with.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
struct HistoryFrameId {
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
impl HistoryFrameId {
    /// Stamp a `HISTORY_TOMBSTONE`: this cursor will never be served again.
    fn tombstone(self, reason: phux_protocol::wire::frame::HistoryTombstoneReason) -> FrameKind {
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
    fn rejected(
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
    fn page(self, page_seq: u64, payload: Bytes, rows: u32, finish: bool) -> FrameKind {
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
fn answer_history(
    reply: tokio::sync::oneshot::Sender<NativeHistoryReply>,
    permit: tokio::sync::mpsc::OwnedPermit<crate::mailbox::Outbound>,
    result: Result<FrameKind, crate::native_state::NativeStateError>,
) {
    let _ = reply.send(NativeHistoryReply { permit, result });
}

/// Map a native-state failure onto the tombstone reason the wire carries.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
const fn history_tombstone_reason(
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
