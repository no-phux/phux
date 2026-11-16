use phux_core::ids::ResourceId;
use phux_protocol::ids::LeaseEpoch;
use phux_protocol::wire::frame::AgentEvent;

use super::{ClientId, ServerState};

impl ServerState {
    /// The client currently holding `pane`'s input lease (ADR-0033), or
    /// `None` if the pane is `Open`.
    #[must_use]
    pub fn input_lease_holder(&self, terminal: ResourceId) -> Option<ClientId> {
        self.leases.holder(terminal)
    }

    /// Whether another client's lease blocks `client`'s input (ADR-0033).
    #[must_use]
    pub fn input_blocked(&self, terminal: ResourceId, client: ClientId) -> bool {
        self.leases.blocked(terminal, client)
    }

    /// Grant `pane`'s input lease to `client` (ADR-0033), returning the prior
    /// holder if the lease was already held (a `Seize` preemption). A change
    /// of holder opens a new epoch and journals `LeaseChanged` (ADR-0163 §7).
    /// Precedence is the caller's: see [`Self::lease_protected_from`].
    pub fn set_input_lease(&mut self, terminal: ResourceId, client: ClientId) -> Option<ClientId> {
        let prior = self.leases.acquire(terminal, client);
        if prior != Some(client) {
            self.journal_lease_changed(terminal, Some(client), Some(client));
        }
        prior
    }

    /// Release the lease if `client` holds it; `true` if released, which
    /// opens a new epoch and journals `LeaseChanged` with no holder.
    pub fn release_input_lease(&mut self, terminal: ResourceId, client: ClientId) -> bool {
        let released = self.leases.release(terminal, client);
        if released {
            self.journal_lease_changed(terminal, None, Some(client));
        }
        released
    }

    /// Release `terminal`'s lease whoever holds it, as the server (a TTL
    /// expiry); the holder it took the lease from, if any.
    pub fn expire_input_lease(&mut self, terminal: ResourceId) -> Option<ClientId> {
        let holder = self.leases.holder(terminal)?;
        if self.leases.release(terminal, holder) {
            self.journal_lease_changed(terminal, None, None);
        }
        Some(holder)
    }

    /// Release every local lease `client` holds (its detach), each in a new
    /// epoch with `LeaseChanged` journaled, and drop its satellite leases.
    /// Both ledgers clear in this one call, or a pane is left that nobody
    /// can type into.
    pub(super) fn release_all_input_leases(&mut self, client: ClientId) {
        for pane in self.leases.release_all_for(client) {
            self.journal_lease_changed(pane, None, Some(client));
        }
        self.federation.release_satellite_leases_for(client);
    }

    /// `terminal`'s current input-lease epoch (ADR-0163 §7): `0` before the
    /// first grant, then one more per change of holder.
    #[must_use]
    pub fn input_lease_epoch(&self, terminal: ResourceId) -> LeaseEpoch {
        LeaseEpoch::new(self.leases.epoch(terminal))
    }

    /// Human precedence (ADR-0163 §6): whether `terminal`'s lease is held by
    /// a human principal and `client` acts as a non-human one, so `client`
    /// may not take it, however deliberately it asks. A human's takeover is
    /// never protected against, and an agent may still take a free lease or
    /// one another agent holds.
    #[must_use]
    pub fn lease_protected_from(&self, terminal: ResourceId, client: ClientId) -> bool {
        self.leases.holder(terminal).is_some_and(|holder| {
            holder != client
                && self.principal_kind(holder).is_human()
                && !self.principal_kind(client).is_human()
        })
    }

    /// The kind of principal `client` acts as: its grant's, or a person for
    /// a connection with none, which [`Self::actor_for`] attributes to the
    /// owner.
    fn principal_kind(&self, client: ClientId) -> phux_core::actor::PrincipalKind {
        self.connection_grant(client).map_or(
            phux_core::actor::PrincipalKind::Human,
            crate::policy::ConnectionGrant::kind,
        )
    }

    /// Open a lease epoch's record: journal `LeaseChanged` for `terminal`
    /// with its new epoch and `holder`, attributed to `actor` (`None` when
    /// the server changed it).
    fn journal_lease_changed(
        &mut self,
        terminal: ResourceId,
        holder: Option<ClientId>,
        actor: Option<ClientId>,
    ) {
        let epoch = self.input_lease_epoch(terminal);
        let Some(resource) = self
            .registry()
            .resource_key(terminal)
            .and_then(crate::org_wire::resource_key)
        else {
            return;
        };
        let wire = self.idspace.intern_resource(terminal);
        crate::audit::emit(
            crate::audit::Kind::InputLease,
            actor.map_or_else(crate::audit::AuditActor::server, |actor| {
                self.audit_actor(actor)
            }),
            if holder.is_some() { "take" } else { "give" },
            Some(crate::audit::terminal_selector(&wire)),
            crate::audit::Outcome::Allowed,
            serde_json::json!({ "epoch": epoch.get(), "client": actor.map(|actor| actor.0) }),
        );
        let holder = holder.map(|holder| self.clients.actor_ref(holder));
        let record = super::EventRecord::new(
            Some(wire),
            AgentEvent::LeaseChanged {
                resource,
                epoch,
                holder,
            },
        )
        .with_actor(actor);
        let _ = self.record_and_fanout(record);
    }

    /// Every pane whose lease `client` holds (for disconnect broadcasts).
    #[must_use]
    pub fn leases_held_by(&self, client: ClientId) -> Vec<ResourceId> {
        self.leases.held_by(client)
    }

    /// Invalidate `terminal`'s expiry generation and, when `scheduled`, mint
    /// a fresh one for a new timer. Called in the same lock scope as the
    /// lease change.
    pub fn refresh_input_lease_expiry(
        &mut self,
        terminal: ResourceId,
        scheduled: bool,
    ) -> Option<u64> {
        self.leases.refresh_expiry(terminal, scheduled)
    }

    /// Whether `generation` is still current (checked by a woken timer).
    #[must_use]
    pub fn input_lease_expiry_is_current(&self, terminal: ResourceId, generation: u64) -> bool {
        self.leases.expiry_is_current(terminal, generation)
    }

    /// Store the spawned timer's abort handle if still current; `false`
    /// tells the caller to abort it.
    #[must_use]
    pub fn attach_input_lease_expiry_abort(
        &mut self,
        terminal: ResourceId,
        generation: u64,
        abort: tokio::task::AbortHandle,
    ) -> bool {
        self.leases.attach_expiry_abort(terminal, generation, abort)
    }

    /// Clear expiry tracking without aborting (the timer's own firing).
    pub fn clear_input_lease_expiry(&mut self, terminal: ResourceId) {
        self.leases.clear_expiry(terminal);
    }

    /// A clone of the live TTL-timer counter (tests).
    #[must_use]
    pub fn input_lease_expiry_task_counter(
        &self,
    ) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        self.leases.expiry_task_counter()
    }
}

#[cfg(test)]
mod tests;
