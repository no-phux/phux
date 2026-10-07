use std::future::Future;

use phux_core::ids::ResourceId;
use phux_protocol::ids::{BootstrapId, ResourceId as WireResourceId};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::resource_table::AttachResourcePumpReplacement;
use super::{ClientId, ServerState};
use crate::mailbox::Outbound;
use crate::resource::ResourceHandle;

impl ServerState {
    /// Current subscribers of `pane`.
    #[must_use]
    pub fn subscribers_for_terminal(&self, terminal: ResourceId) -> &[ClientId] {
        self.resources.subscribers_for(terminal)
    }

    /// Subscribe `client` to `terminal` (deduplicated). Pass `mailbox` for
    /// clients with no session `ATTACH` behind them (`ATTACH_RESOURCE`) so
    /// terminal-scoped fanout reaches them; `None` only for attached ones.
    pub fn subscribe_terminal(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
        mailbox: Option<mpsc::Sender<Outbound>>,
    ) {
        if let Some(tx) = mailbox {
            self.clients.remember_terminal_mailbox(client, tx);
        }
        self.resources.subscribe(client, terminal);
    }

    /// One client's mailbox, resolved like
    /// [`Self::terminal_fanout_targets`] (attached first).
    #[must_use]
    pub fn client_mailbox(&self, client: ClientId) -> Option<mpsc::Sender<Outbound>> {
        self.clients.terminal_fanout_mailbox(client).cloned()
    }

    /// Every subscriber's mailbox for terminal-scoped fanout
    /// (`RESOURCE_CLOSED`, L1 §3.1), one per client whether attached or
    /// `ATTACH_RESOURCE`-only. Closed mailboxes are skipped.
    #[must_use]
    pub fn terminal_fanout_targets(&self, terminal: ResourceId) -> Vec<mpsc::Sender<Outbound>> {
        self.subscribers_for_terminal(terminal)
            .iter()
            .filter_map(|client| self.clients.terminal_fanout_mailbox(*client).cloned())
            .collect()
    }

    /// Handles of every pane `client_id` subscribes to, gathered under the
    /// lock so detach requests can be sent off it.
    #[must_use]
    pub fn subscribed_resource_handles(&self, client_id: ClientId) -> Vec<ResourceHandle> {
        self.resources.subscribed_handles(client_id)
    }

    /// Record a spawned engine's handle and token and allocate its wire id
    /// (idempotent on the id; overwrites the handle). Does not spawn; see
    /// [`Self::spawn_resource_actor`].
    pub fn register_resource_handle(
        &mut self,
        terminal: ResourceId,
        handle: ResourceHandle,
        token: CancellationToken,
    ) -> WireResourceId {
        let wire = self.intern_terminal_wire(terminal);
        self.resources.register(terminal, handle, token);
        wire
    }

    /// Register `handle`/`token` and spawn `actor_future` on the pane
    /// `JoinSet` (inside a `LocalSet`, ADR-0014). Returns the wire id.
    pub fn spawn_resource_actor<F>(
        &mut self,
        terminal: ResourceId,
        handle: ResourceHandle,
        token: CancellationToken,
        actor_future: F,
    ) -> WireResourceId
    where
        F: Future<Output = ()> + 'static,
    {
        let wire = self.register_resource_handle(terminal, handle, token);
        self.resources.spawn_actor(actor_future);
        wire
    }

    /// Cancel every output pump of `terminal`: a killed Terminal's process
    /// outlives its close (the hangup grace), and nothing it prints then may
    /// follow its `RESOURCE_CLOSED`.
    pub(crate) fn cancel_terminal_pumps(&mut self, terminal: ResourceId) {
        self.resources.cancel_pumps_for_terminal(terminal);
    }

    /// Cancel and forget `pane`'s actor token. Idempotent.
    pub fn detach_resource_actor(&mut self, terminal: ResourceId) {
        self.resources.detach_actor(terminal);
    }

    /// `terminal`'s handle, if registered.
    #[must_use]
    pub fn resource_handle(&self, terminal: ResourceId) -> Option<&ResourceHandle> {
        self.resources.handle(terminal)
    }

    /// Every `(pane, handle)`, cloned for use off the lock.
    pub(crate) fn all_resource_handles(&self) -> Vec<(ResourceId, ResourceHandle)> {
        self.resources.all_handles()
    }

    /// Every registered resource id (no handle clones).
    #[must_use]
    pub fn resource_ids(&self) -> Vec<ResourceId> {
        self.resources.resource_ids()
    }

    /// Install a new `ATTACH_RESOURCE` pump generation, displacing any live
    /// one (see `ResourceTable::replace_pump`).
    pub fn replace_attach_terminal_pump(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
        bootstrap_id: BootstrapId,
    ) -> AttachResourcePumpReplacement {
        self.resources.replace_pump(client, terminal, bootstrap_id)
    }

    /// The next per-terminal bootstrap id for `client`; `None` when
    /// exhausted.
    pub fn next_attach_terminal_bootstrap_id(&mut self, client: ClientId) -> Option<BootstrapId> {
        self.resources.next_bootstrap_id(client)
    }

    /// Cancel the pump for `(client, terminal)`. Idempotent.
    pub fn cancel_attach_terminal_pump(&mut self, client: ClientId, terminal: ResourceId) {
        self.resources.cancel_pump(client, terminal);
    }

    /// Track a task from any of the three output subscription paths.
    pub(crate) fn track_terminal_output_pump(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
        abort: tokio::task::AbortHandle,
        done: CancellationToken,
    ) {
        self.resources
            .track_output_pump(client, terminal, abort, done);
    }

    /// Abort this subscription's tasks; await the returned tokens off the
    /// lock before acknowledging.
    pub(crate) fn stop_terminal_output_pumps(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
    ) -> Vec<CancellationToken> {
        self.resources.stop_output_pumps(client, terminal)
    }

    /// Remove `client` from `terminal`'s subscribers.
    pub fn unsubscribe_terminal(&mut self, client: ClientId, terminal: ResourceId) {
        self.resources.unsubscribe(client, terminal);
    }

    /// Record that `client` spawned `terminal` (ADR-0109), before anything
    /// subscribes to it.
    pub fn record_spawn(&mut self, terminal: ResourceId, client: ClientId) {
        self.resources.record_spawn(terminal, client);
    }
}
