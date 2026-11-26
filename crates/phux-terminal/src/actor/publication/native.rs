//! Exact native activation and history-retain ownership.

use std::cell::RefCell;

use super::PublicationOwner;
use crate::actor::{
    CanonicalTerminal, NativeCaptureInterest, NativeCursorKey, NativeCursorOwner,
    NativePublicationReply, NativePublicationRequest, ResourceOutput,
};

impl PublicationOwner {
    pub(in crate::actor) fn handle_native_publication(
        &mut self,
        req: NativePublicationRequest,
        terminal: &RefCell<CanonicalTerminal>,
        output: &tokio::sync::broadcast::Sender<ResourceOutput>,
    ) {
        // A queued request is fenced by its original capture lifetime before
        // any reply-loss cleanup can touch a binding with repeated identities.
        if req.cancellation.is_cancelled() {
            let _ = req
                .reply
                .send(Err(crate::native_state::NativeStateError::InvalidHandle));
            return;
        }
        let key = NativeCursorKey::new(req.owner, req.stream_id);
        let valid = self.native_cursor_owners.get(&key).is_some_and(|binding| {
            !binding.cancellation.is_cancelled()
                && binding.cursor == req.cursor
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
        if req.reply.is_closed()
            || self.consumers.iter().any(|(client, consumer)| {
                u64::from(client.get()) == req.owner
                    && consumer.sync.stream_id == req.stream_id
                    && consumer.cancellation.is_cancelled()
            })
        {
            self.unbind_native_cursor(key, terminal);
            return;
        }
        if self
            .native_cursor_owners
            .get(&key)
            .is_some_and(|binding| binding.superseded)
        {
            self.unbind_native_cursor(key, terminal);
            let _ = req
                .reply
                .send(Err(crate::native_state::NativeStateError::Resize));
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
        let live = output.subscribe();
        let remove = publication.waiting.is_empty();
        if remove {
            self.native_publications.remove(&req.cursor);
        }
        if self
            .native_cursor_owners
            .get(&key)
            .is_some_and(|binding| binding.history_released)
            && let Some(binding) = self.native_cursor_owners.remove(&key)
        {
            binding.cancellation.cancel();
        }
        let interest = NativeCaptureInterest::default();
        if let Some(binding) = self.native_cursor_owners.get_mut(&key) {
            binding.cancellation.cancel();
            binding.cancellation = interest.cancellation();
            self.watch_native(interest.cancellation(), key, req.bootstrap_id, req.cursor);
        }
        if req
            .reply
            .send(Ok(NativePublicationReply {
                replay,
                live,
                interest,
            }))
            .is_err()
        {
            self.unbind_native_cursor(key, terminal);
        }
    }

    pub(in crate::actor) fn unbind_native_cursor(
        &mut self,
        key: NativeCursorKey,
        terminal: &RefCell<CanonicalTerminal>,
    ) -> Option<NativeCursorOwner> {
        let binding = self.native_cursor_owners.remove(&key)?;
        binding.cancellation.cancel();
        if let Some(publication) = self.native_publications.get_mut(&binding.cursor) {
            publication.waiting.remove(&key);
            if publication.waiting.is_empty() {
                self.native_publications.remove(&binding.cursor);
            }
        }
        Self::release_history(terminal, &binding);
        Some(binding)
    }

    pub(in crate::actor) fn release_history(
        terminal: &RefCell<CanonicalTerminal>,
        binding: &NativeCursorOwner,
    ) {
        if !binding.history_released
            && let CanonicalTerminal::Native(manager) = &mut *terminal.borrow_mut()
        {
            let _ = manager.release_generation(&binding.cursor);
        }
    }
}
