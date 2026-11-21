use std::future::Future;

use phux_core::ids::ResourceId;
use phux_protocol::ids::ResourceId as WireResourceId;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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
    /// Everything keyed on a live pane: handles, tokens, subscriptions, and
    /// output pumps. Registering an engine checks its kind and interns its
    /// wire id, so it goes through [`Self::register_resource_handle`].
    #[must_use]
    pub(crate) const fn resources(&self) -> &super::ResourceTable {
        &self.resources
    }

    /// Mutable [`Self::resources`].
    pub(crate) const fn resources_mut(&mut self) -> &mut super::ResourceTable {
        &mut self.resources
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
        self.resources
            .subscribers_for(terminal)
            .iter()
            .filter_map(|client| self.clients.terminal_fanout_mailbox(*client).cloned())
            .collect()
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
        let wire = self.idspace.intern_resource(terminal);
        self.resources.register(terminal, handle, token);
        // The member's leaf is live now; legacy layout readers see it.
        if let Some(session) = self.registry().session_of(terminal) {
            self.publish_arrangement_as_layout(session);
        }
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

    /// The mailbox of the connection that spawned `resource`, a document's
    /// producer, while it is connected; `None` once it is gone.
    #[must_use]
    pub fn resource_producer(&self, resource: ResourceId) -> Option<mpsc::Sender<Outbound>> {
        self.resources
            .spawner(resource)
            .and_then(|client| self.client_mailbox(client))
            .filter(|tx| !tx.is_closed())
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
        self.resources
            .handle(resource)?
            .companion::<phux_protocol::wire::frame::ControlClass>()
            .copied()
    }
}
