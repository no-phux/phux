//! Finishing a native bootstrap: replies, generations, replay buffering.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

impl TerminalActor {
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn native_bootstrap_reply(
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
                codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1,
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
    /// reservation, and notify every waiter, unwinding on failure so each
    /// waiter is answered exactly once.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn finish_native_bootstrap(
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
    pub(in crate::actor) fn prepare_native_bootstrap_replies(
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
    pub(in crate::actor) fn install_native_generation(
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
    pub(in crate::actor) fn native_publication_conflicts(
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
    pub(in crate::actor) fn register_native_bootstrap_waiters(
        &mut self,
        prepared: Vec<(NativeBootstrapRequest, NativeBootstrapReply)>,
        cursor: crate::native_state::OpaqueHistoryCursor,
        installed_new: bool,
    ) -> HashSet<NativeCursorKey> {
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
            let key = NativeCursorKey::new(waiter.owner, waiter.stream_id);
            self.native_cursor_owners.insert(
                key,
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
            waiting.insert(key);
            if waiter.reply.send(Ok(reply_value)).is_err() {
                waiting.remove(&key);
                self.release_native_binding(key);
            }
        }
        waiting
    }

    /// Charge one waiter's retain against the generation. `rides_install` is
    /// the first waiter on a freshly installed generation: the install itself
    /// already retained it, so it takes no second retain.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn retain_native_generation(
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
    pub(in crate::actor) fn record_native_publication(
        &mut self,
        cursor: crate::native_state::OpaqueHistoryCursor,
        base_seq: u64,
        replay: VecDeque<(u64, Bytes)>,
        replay_bytes: usize,
        waiting: HashSet<NativeCursorKey>,
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
    pub(in crate::actor) fn fail_native_bootstrap(
        &mut self,
        pending: PendingNativeBootstrap,
        error: crate::native_state::NativeStateError,
    ) {
        self.abort_native_bootstrap(pending, error);
        self.start_next_native_bootstrap();
    }

    /// Return the loaned terminal and answer this capture without starting
    /// another loan. EOF snapshots need canonical access before work resumes.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn abort_native_bootstrap(
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
            // `Resize` survives the collapse: the cut was invalidated, not
            // refused, and the waiter's pump waits for the resync that follows.
            let error = match error {
                crate::native_state::NativeStateError::OutOfMemory
                | crate::native_state::NativeStateError::Resize => error,
                _ => crate::native_state::NativeStateError::LimitExceeded,
            };
            let _ = waiter.reply.send(Err(error));
        }
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn start_next_native_bootstrap(&mut self) {
        if self.pending_native_bootstrap.is_none()
            && let Some(next) = self.native_bootstrap_backlog.pop_front()
        {
            self.start_native_bootstrap(next);
        }
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn apply_native_replay(&mut self, replay: &VecDeque<(u64, Bytes)>) {
        if replay.is_empty() {
            return;
        }
        for (_, bytes) in replay {
            self.terminal.borrow_mut().vt_write(bytes);
            self.source_events();
        }
        self.publish_input_snapshot();
        self.terminal_dirty_since_tick = true;
        self.agent_dirty_since_detect = true;
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn buffer_native_live_output(&mut self, seq: u64, bytes: &Bytes) -> bool {
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
                for key in publication.waiting {
                    self.release_native_binding(key);
                }
            }
        }
        deferred
    }
}
