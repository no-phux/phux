//! Remote listener bind outcomes on shared state (phux-kyna).

use phux_protocol::wire::{RemoteListenerSlot, RemoteListenersReport};

use super::ServerState;

impl ServerState {
    /// Record (or replace) one remote listener bind outcome.
    pub fn record_remote_listener(&mut self, slot: RemoteListenerSlot) {
        self.remote_listeners.upsert(slot);
    }

    /// The listener report filled so far this process.
    #[must_use]
    pub const fn remote_listeners(&self) -> &RemoteListenersReport {
        &self.remote_listeners
    }

    /// Whether any remote listener slot has been recorded yet.
    #[must_use]
    pub fn has_remote_listener_report(&self) -> bool {
        !self.remote_listeners.listeners.is_empty()
    }
}
