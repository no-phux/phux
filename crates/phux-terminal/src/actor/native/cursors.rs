//! Native cursor ownership and invalidation.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

impl TerminalActor {
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn publish_native_control(&self, owner: u64, frame: FrameKind) {
        let _ = self
            .core
            .output_tx
            .send(PaneOutput::Control { owner, frame });
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    ///
    /// Returns the pumps it tombstoned, so a reflow that schedules no
    /// everyone-resync can still address one to them (phux-p5bo).
    pub(in crate::actor) fn invalidate_all_native_cursors(
        &mut self,
        reason: phux_protocol::wire::frame::TombstoneReason,
    ) -> Vec<phux_resource::ResyncTarget> {
        if let Some(pending) = self.pending_native_history.take() {
            answer_released_history(pending.request);
        }
        for pending in self.native_history_backlog.drain(..) {
            answer_released_history(pending.request);
        }
        self.native_bootstrap_backlog.clear();
        if let Some(pending) = self.pending_native_bootstrap.take() {
            self.fail_native_bootstrap(pending, crate::native_state::NativeStateError::Resize);
        }
        let bindings: Vec<_> = self.native_cursor_owners.drain().collect();
        self.native_publications.clear();
        let last_valid_seq = self.core.seq();
        let mut tombstoned = Vec::with_capacity(bindings.len());
        for (key, binding) in bindings {
            tombstoned.push(phux_resource::ResyncTarget {
                owner: key.owner,
                stream_id: binding.stream_id,
                bootstrap_id: binding.bootstrap_id,
            });
            self.publish_native_control(
                key.owner,
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
    pub(in crate::actor) fn invalidate_native_owner(
        &mut self,
        owner: u64,
        stream_id: phux_protocol::ids::StreamId,
        reason: phux_protocol::wire::frame::TombstoneReason,
    ) {
        let key = NativeCursorKey::new(owner, stream_id);
        self.cancel_native_history_requests(
            owner,
            Some(stream_id),
            phux_protocol::wire::frame::HistoryTombstoneReason::Released,
        );
        self.drop_native_bootstrap_waiters(owner, Some(stream_id));
        let Some(binding) = self.unbind_native_cursor(key) else {
            return;
        };
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
    }

    /// Release every native binding owned by a detached client.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn release_native_owner(&mut self, owner: u64) {
        self.cancel_native_history_requests(
            owner,
            None,
            phux_protocol::wire::frame::HistoryTombstoneReason::Released,
        );
        self.drop_native_bootstrap_waiters(owner, None);
        let keys: Vec<_> = self
            .native_cursor_owners
            .keys()
            .filter(|key| key.owner == owner)
            .copied()
            .collect();
        for key in keys {
            let _ = self.unbind_native_cursor(key);
        }
    }

    /// Release one pump's binding without touching sibling streams of the
    /// same client.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn release_native_binding(&mut self, key: NativeCursorKey) {
        self.cancel_native_history_requests(
            key.owner,
            Some(key.stream_id),
            phux_protocol::wire::frame::HistoryTombstoneReason::Released,
        );
        self.drop_native_bootstrap_waiters(key.owner, Some(key.stream_id));
        let _ = self.unbind_native_cursor(key);
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn drop_native_bootstrap_waiters(
        &mut self,
        owner: u64,
        stream_id: Option<phux_protocol::ids::StreamId>,
    ) {
        if let Some(pending) = self.pending_native_bootstrap.as_mut() {
            pending
                .waiters
                .retain(|waiter| keeps_native_bootstrap_waiter(waiter, owner, stream_id));
        }
        self.native_bootstrap_backlog
            .retain(|waiter| keeps_native_bootstrap_waiter(waiter, owner, stream_id));
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn unbind_native_cursor(
        &mut self,
        key: NativeCursorKey,
    ) -> Option<NativeCursorOwner> {
        let binding = self.native_cursor_owners.remove(&key)?;
        if let Some(publication) = self.native_publications.get_mut(&binding.cursor) {
            publication.waiting.remove(&key);
            if publication.waiting.is_empty() {
                self.native_publications.remove(&binding.cursor);
            }
        }
        if let CanonicalTerminal::Native(manager) = &mut *self.terminal.borrow_mut() {
            let _ = manager.release_generation(&binding.cursor);
        }
        Some(binding)
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn cancel_native_history_requests(
        &mut self,
        owner: u64,
        stream_id: Option<phux_protocol::ids::StreamId>,
        reason: phux_protocol::wire::frame::HistoryTombstoneReason,
    ) {
        if self
            .pending_native_history
            .as_ref()
            .is_some_and(|pending| matches_native_history(&pending.request, owner, stream_id))
            && let Some(pending) = self.pending_native_history.take()
        {
            answer_tombstoned_history(pending.request, reason);
        }
        let mut retained = VecDeque::with_capacity(self.native_history_backlog.len());
        while let Some(pending) = self.native_history_backlog.pop_front() {
            if matches_native_history(&pending.request, owner, stream_id) {
                answer_tombstoned_history(pending.request, reason);
            } else {
                retained.push_back(pending);
            }
        }
        self.native_history_backlog = retained;
        if self.pending_native_history.is_none() {
            self.start_next_native_history();
        }
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(in crate::actor) fn expire_native_cursors(&mut self) {
        let cutoff = tokio::time::Instant::now() - NATIVE_HISTORY_TTL;
        let keys: Vec<_> = self
            .native_cursor_owners
            .iter()
            .filter_map(|(key, binding)| (binding.touched <= cutoff).then_some(*key))
            .collect();
        for key in keys {
            self.cancel_native_history_requests(
                key.owner,
                Some(key.stream_id),
                phux_protocol::wire::frame::HistoryTombstoneReason::Expired,
            );
            let Some(binding) = self.native_cursor_owners.get(&key) else {
                continue;
            };
            self.publish_native_control(
                key.owner,
                FrameKind::HistoryTombstone {
                    terminal_id: binding.terminal_id.clone(),
                    stream_id: binding.stream_id,
                    bootstrap_id: binding.bootstrap_id,
                    cursor: Bytes::copy_from_slice(&binding.cursor),
                    reason: phux_protocol::wire::frame::HistoryTombstoneReason::Expired,
                },
            );
            self.release_native_binding(key);
        }
    }
}
