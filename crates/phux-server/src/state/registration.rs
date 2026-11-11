use std::future::Future;

use phux_core::ids::ResourceId;
use phux_protocol::ids::{BootstrapId, ResourceId as WireResourceId};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::resource_table::AttachResourcePumpReplacement;
use super::{ClientId, ServerState};
use crate::mailbox::Outbound;
use crate::resource::{ResourceHandle, ResourceKind};

#[cfg(test)]
#[path = "tests/registration.rs"]
mod tests;

/// Why an engine cannot be published under a resource record. Refusal
/// leaves registrations and wire identities unchanged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResourceRegistrationError {
    /// There is no resource record to own the engine.
    #[error("resource {0:?} has no record for its engine")]
    NotFound(ResourceId),
    /// The proposed engine contradicts the persistent resource kind.
    #[error("resource {resource:?} is {expected}, but its engine is {actual}")]
    WrongKind {
        /// Resource whose identity must be preserved.
        resource: ResourceId,
        /// Kind recorded in the organization.
        expected: ResourceKind,
        /// Kind served by the proposed engine.
        actual: ResourceKind,
    },
}

impl ServerState {
    /// Current subscribers of `pane`.
    #[must_use]
    pub fn subscribers_for_resource(&self, terminal: ResourceId) -> &[ClientId] {
        self.resources.subscribers_for(terminal)
    }

    /// Subscribe `client` to `terminal` (deduplicated). Pass `mailbox` for
    /// clients with no session `ATTACH` behind them (`ATTACH_RESOURCE`) so
    /// terminal-scoped fanout reaches them; `None` only for attached ones.
    pub fn subscribe_resource(
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
        self.subscribers_for_resource(terminal)
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
    ///
    /// # Errors
    ///
    /// No resource record exists, or its kind differs from the handle's.
    pub fn register_resource_handle(
        &mut self,
        terminal: ResourceId,
        handle: ResourceHandle,
        token: CancellationToken,
    ) -> Result<WireResourceId, ResourceRegistrationError> {
        let expected = self
            .registry()
            .record(terminal)
            .ok_or(ResourceRegistrationError::NotFound(terminal))?
            .kind;
        if expected != handle.kind {
            return Err(ResourceRegistrationError::WrongKind {
                resource: terminal,
                expected,
                actual: handle.kind,
            });
        }
        let wire = self.intern_resource_wire(terminal);
        self.resources.register(terminal, handle, token);
        Ok(wire)
    }

    /// Register `handle`/`token` and spawn `actor_future` on the pane
    /// `JoinSet` (inside a `LocalSet`, ADR-0014). Returns the wire id.
    ///
    /// # Errors
    ///
    /// Registration is refused before the actor future can run; see
    /// [`Self::register_resource_handle`].
    pub fn spawn_resource_actor<F>(
        &mut self,
        terminal: ResourceId,
        handle: ResourceHandle,
        token: CancellationToken,
        actor_future: F,
    ) -> Result<WireResourceId, ResourceRegistrationError>
    where
        F: Future<Output = ()> + 'static,
    {
        let wire = self.register_resource_handle(terminal, handle, token)?;
        self.resources.spawn_actor(actor_future);
        Ok(wire)
    }

    /// Shutdown: cancel every engine and hand back all engine futures, so
    /// the caller can let each finish its teardown before the `LocalSet`
    /// drops.
    pub(crate) fn retire_engines(&mut self) -> tokio::task::JoinSet<()> {
        self.resources.retire_engines()
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

    /// Relabel the current `ATTACH_RESOURCE` pump that owns `last_valid_seq`
    /// for a replacement generation (see
    /// `ResourceTable::advance_pump_generation`).
    pub(crate) fn advance_attach_terminal_pump(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
        last_valid_seq: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Option<BootstrapId> {
        self.resources
            .advance_pump_generation(client, terminal, last_valid_seq)
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
        drain: Option<CancellationToken>,
    ) {
        self.resources
            .track_output_pump(client, terminal, abort, done, drain);
    }

    /// Signal natural-close drain and capture each client's completion fences.
    pub(crate) fn begin_terminal_output_drain(
        &mut self,
        terminal: ResourceId,
        wire: phux_protocol::ids::ResourceId,
    ) -> Vec<(mpsc::Sender<Outbound>, Vec<CancellationToken>)> {
        self.resources.remember_draining_terminal(wire, terminal);
        self.resources
            .begin_output_drain(terminal)
            .into_iter()
            .filter_map(|(client, done)| self.client_mailbox(client).map(|tx| (tx, done)))
            .collect()
    }

    pub(crate) fn draining_resource_from_wire(
        &self,
        wire: &phux_protocol::ids::ResourceId,
    ) -> Option<ResourceId> {
        self.resources.draining_terminal(wire)
    }

    pub(crate) fn finish_terminal_output_drain(&mut self, terminal: ResourceId) {
        self.resources.finish_output_drain(terminal);
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
    pub fn unsubscribe_resource(&mut self, client: ClientId, terminal: ResourceId) {
        self.resources.unsubscribe(client, terminal);
    }

    /// The mailbox of the connection that spawned `resource`, a document's
    /// producer, while it is connected; `None` once it is gone.
    #[must_use]
    pub fn resource_producer(&self, resource: ResourceId) -> Option<mpsc::Sender<Outbound>> {
        self.resources
            .spawner(resource)
            .and_then(|client| self.client_mailbox(client))
            .filter(|tx| !tx.is_closed())
    }

    /// The connection that spawned (produces) `resource`, when a client did.
    #[must_use]
    pub fn resource_spawner(&self, resource: ResourceId) -> Option<ClientId> {
        self.resources.spawner(resource)
    }

    /// What input to `resource` controls, as its producer declared at spawn
    /// with a [`ControlClass`](phux_protocol::wire::frame::ControlClass)
    /// companion (`docs/resource-kinds.md#authority`); `None` when it
    /// controls nothing.
    #[must_use]
    pub fn resource_control(
        &self,
        resource: ResourceId,
    ) -> Option<phux_protocol::wire::frame::ControlClass> {
        self.resource_handle(resource)?
            .companion::<phux_protocol::wire::frame::ControlClass>()
            .copied()
    }

    /// Record that `client` spawned `terminal` (ADR-0109), before anything
    /// subscribes to it.
    pub fn record_spawn(&mut self, terminal: ResourceId, client: ClientId) {
        self.resources.record_spawn(terminal, client);
    }
}
