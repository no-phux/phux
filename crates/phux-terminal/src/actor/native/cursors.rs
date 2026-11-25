//! Native cursor ownership and invalidation.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

impl TerminalActor {
    pub(in crate::actor) fn publish_native_control(&self, owner: u64, frame: FrameKind) {
        let _ = self
            .core
            .output_tx
            .send(ResourceOutput::Control { owner, frame });
    }

    ///
    /// Returns the pumps it tombstoned, so a reflow that schedules no
    /// everyone-resync can still address one to them (phux-p5bo).
    pub(in crate::actor) fn invalidate_all_native_cursors(
        &mut self,
        reason: phux_protocol::wire::frame::TombstoneReason,
    ) -> Vec<phux_resource::ResyncTarget> {
        if let Some(pending) = self.publication.pending_native_history.take() {
            answer_released_history(pending.request);
        }
        for pending in self.publication.native_history_backlog.drain(..) {
            answer_released_history(pending.request);
        }
        self.publication.native_bootstrap_backlog.clear();
        if let Some(pending) = self.publication.pending_native_bootstrap.take() {
            self.fail_native_bootstrap(pending, crate::native_state::NativeStateError::Resize);
        }
        let waiting: HashSet<_> = self.publication
            .native_publications
            .values()
            .flat_map(|publication| publication.waiting.iter().copied())
            .collect();
        let bindings: Vec<_> = self.publication.native_cursor_owners.drain().collect();
        self.publication.native_publications.clear();
        let last_valid_seq = self.core.seq();
        let mut tombstoned = Vec::with_capacity(bindings.len());
        for (key, binding) in bindings {
            if let Some(target) = self.retire_native_binding(
                key,
                binding,
                reason,
                last_valid_seq,
                waiting.contains(&key),
            ) {
                tombstoned.push(target);
            }
        }
        tombstoned
    }

    fn retire_native_binding(
        &mut self,
        key: NativeCursorKey,
        mut binding: NativeCursorOwner,
        reason: phux_protocol::wire::frame::TombstoneReason,
        last_valid_seq: u64,
        waiting: bool,
    ) -> Option<phux_resource::ResyncTarget> {
        let target = (!binding.superseded).then_some(phux_resource::ResyncTarget {
            owner: key.owner,
            stream_id: binding.stream_id,
            bootstrap_id: binding.bootstrap_id,
        });
        if target.is_some() {
            self.publish_native_control(
                key.owner,
                FrameKind::BootstrapTombstone {
                    terminal_id: binding.terminal_id.clone(),
                    stream_id: binding.stream_id,
                    bootstrap_id: binding.bootstrap_id,
                    reason,
                    last_valid_seq,
                },
            );
        }
        super::super::publication::PublicationOwner::release_history(&self.terminal, &binding);
        if reason == phux_protocol::wire::frame::TombstoneReason::Resize
            && (waiting || binding.superseded)
        {
            binding.history_released = true;
            binding.superseded = true;
            self.publication.native_cursor_owners.insert(key, binding);
        } else {
            binding.cancellation.cancel();
        }
        target
    }

    /// FINISH ends progressive history, not the independent publication right.
    pub(in crate::actor) fn finish_native_history(&mut self, key: NativeCursorKey) {
        let Some(mut binding) = self.publication.native_cursor_owners.remove(&key) else {
            return;
        };
        super::super::publication::PublicationOwner::release_history(&self.terminal, &binding);
        binding.history_released = true;
        let waiting = self.publication
            .native_publications
            .get(&binding.cursor)
            .is_some_and(|publication| publication.waiting.contains(&key));
        if waiting || binding.superseded {
            self.publication.native_cursor_owners.insert(key, binding);
        } else {
            binding.cancellation.cancel();
        }
    }

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
        if binding.superseded {
            return;
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
    }

    /// Release every native binding owned by a detached client.
    pub(in crate::actor) fn release_native_owner(&mut self, owner: u64) {
        self.cancel_native_history_requests(
            owner,
            None,
            phux_protocol::wire::frame::HistoryTombstoneReason::Released,
        );
        self.drop_native_bootstrap_waiters(owner, None);
        // At most eight bindings; remove directly instead of allocating a
        // teardown vector for every cancelled connection.
        while let Some(key) = self.publication.native_cursor_owners.keys().copied().find(|key| key.owner == owner) {
            let _ = self.unbind_native_cursor(key);
        }
    }

    /// Release one pump's binding without touching sibling streams of the
    /// same client.
    pub(in crate::actor) fn release_native_binding(&mut self, key: NativeCursorKey) {
        self.cancel_native_history_requests(
            key.owner,
            Some(key.stream_id),
            phux_protocol::wire::frame::HistoryTombstoneReason::Released,
        );
        self.drop_native_bootstrap_waiters(key.owner, Some(key.stream_id));
        let _ = self.unbind_native_cursor(key);
    }

    /// Registration rollback is scoped to the replica, not merely its client
    /// or stream: a queued old withdrawal must leave a recapture intact.
    pub(in crate::actor) fn release_native_consumer_generation(
        &mut self,
        owner: u64,
        stream_id: phux_protocol::ids::StreamId,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) {
        let keep = |waiter: &NativeBootstrapRequest| {
            waiter.owner != owner
                || waiter.stream_id != stream_id
                || waiter.bootstrap_id != bootstrap_id
        };
        if let Some(pending) = self.publication.pending_native_bootstrap.as_mut() {
            pending.waiters.retain(keep);
        }
        self.publication.native_bootstrap_backlog.retain(keep);
        let key = NativeCursorKey::new(owner, stream_id);
        if self.publication
            .native_cursor_owners
            .get(&key)
            .is_some_and(|binding| binding.bootstrap_id == bootstrap_id)
        {
            self.cancel_native_history_requests(
                owner,
                Some(stream_id),
                phux_protocol::wire::frame::HistoryTombstoneReason::Released,
            );
            let _ = self.unbind_native_cursor(key);
        }
    }

    pub(in crate::actor) fn drop_native_bootstrap_waiters(
        &mut self,
        owner: u64,
        stream_id: Option<phux_protocol::ids::StreamId>,
    ) {
        if let Some(pending) = self.publication.pending_native_bootstrap.as_mut() {
            pending
                .waiters
                .retain(|waiter| keeps_native_bootstrap_waiter(waiter, owner, stream_id));
        }
        self.publication.native_bootstrap_backlog
            .retain(|waiter| keeps_native_bootstrap_waiter(waiter, owner, stream_id));
    }

    pub(in crate::actor) fn unbind_native_cursor(&mut self, key: NativeCursorKey) -> Option<NativeCursorOwner> {
        self.publication.unbind_native_cursor(key, &self.terminal)
    }

    pub(in crate::actor) fn cancel_native_history_requests(
        &mut self,
        owner: u64,
        stream_id: Option<phux_protocol::ids::StreamId>,
        reason: phux_protocol::wire::frame::HistoryTombstoneReason,
    ) {
        if self.publication
            .pending_native_history
            .as_ref()
            .is_some_and(|pending| matches_native_history(&pending.request, owner, stream_id))
            && let Some(pending) = self.publication.pending_native_history.take()
        {
            answer_tombstoned_history(pending.request, reason);
        }
        // Rotate the existing bounded queue in place; cancellation does not
        // allocate another backlog just to retain its unaffected requests.
        for _ in 0..self.publication.native_history_backlog.len() {
            let Some(pending) = self.publication.native_history_backlog.pop_front() else {
                break;
            };
            if matches_native_history(&pending.request, owner, stream_id) {
                answer_tombstoned_history(pending.request, reason);
            } else {
                self.publication.native_history_backlog.push_back(pending);
            }
        }
        if self.publication.pending_native_history.is_none() {
            self.start_next_native_history();
        }
    }

    pub(in crate::actor) fn expire_native_cursors(&mut self) {
        let cutoff = tokio::time::Instant::now() - NATIVE_HISTORY_TTL;
        while let Some(key) = self.publication.native_cursor_owners.iter()
            .find_map(|(key, binding)| (binding.touched <= cutoff).then_some(*key))
        {
            self.cancel_native_history_requests(
                key.owner,
                Some(key.stream_id),
                phux_protocol::wire::frame::HistoryTombstoneReason::Expired,
            );
            let Some(binding) = self.publication.native_cursor_owners.get(&key) else {
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
