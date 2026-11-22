//! Remote listener bind outcomes on shared state (phux-kyna).

use std::sync::Arc;

use phux_protocol::wire::{RemoteListenerSlot, RemoteListenerTransport, RemoteListenersReport};

use crate::transport::quic::ReplyFailures;

use super::ServerState;

impl ServerState {
    /// Record (or replace) one remote listener bind outcome.
    pub fn record_remote_listener(&mut self, slot: RemoteListenerSlot) {
        self.remote_listeners.upsert(slot);
    }

    /// Watch the configured QUIC listener's replies, so the report says when
    /// they cannot leave the host.
    pub(crate) fn watch_quic_replies(&mut self, replies: Arc<ReplyFailures>) {
        self.quic_replies = Some(replies);
    }

    /// The listener report filled so far this process, with any recent
    /// reply failure on the QUIC row.
    #[must_use]
    pub fn remote_listeners(&self) -> RemoteListenersReport {
        let mut report = self.remote_listeners.clone();
        let failure = self
            .quic_replies
            .as_ref()
            .and_then(|replies| replies.recent());
        if let Some(slot) = report
            .listeners
            .iter_mut()
            .find(|slot| slot.bound && slot.transport == RemoteListenerTransport::Quic)
        {
            slot.reply_failure = failure;
        }
        report
    }

    /// Whether any remote listener slot has been recorded yet.
    #[must_use]
    pub const fn has_remote_listener_report(&self) -> bool {
        !self.remote_listeners.listeners.is_empty()
    }
}
