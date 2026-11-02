use phux_core::ids::ResourceId;
use phux_protocol::ids::LeaseEpoch;
use phux_protocol::wire::frame::{ActorRef, AgentEvent};
use tokio::sync::mpsc;

use super::{ClientId, Outbound, SatelliteLease, ServerState};

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
    pub(super) fn release_all_input_leases(&mut self, client: ClientId) {
        for pane in self.leases.release_all_for(client) {
            self.journal_lease_changed(pane, None, Some(client));
        }
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

    /// Who held `resource`'s input lease at any time in `[from_ms, to_ms]`
    /// (Unix milliseconds), answered from the journal's `LeaseChanged`
    /// events without any keystroke log (ADR-0163 §7).
    #[must_use]
    pub fn lease_evidence(
        &self,
        resource: &phux_protocol::ids::ResourceKey,
        from_ms: u64,
        to_ms: u64,
    ) -> LeaseEvidence {
        let mut epochs: Vec<LeaseInterval> = Vec::new();
        for entry in self.journal.retained() {
            let AgentEvent::LeaseChanged {
                resource: changed,
                epoch,
                holder,
            } = &entry.event
            else {
                continue;
            };
            if changed != resource {
                continue;
            }
            let at = entry.stamp.ts_ms;
            if let Some(open) = epochs.last_mut() {
                open.until_ms = Some(at);
            }
            epochs.push(LeaseInterval {
                epoch: *epoch,
                holder: holder.clone(),
                from_ms: at,
                until_ms: None,
            });
        }
        let intervals = epochs
            .into_iter()
            .filter(|interval| {
                interval.holder.is_some()
                    && interval.from_ms <= to_ms
                    && interval.until_ms.is_none_or(|until| until >= from_ms)
            })
            .collect();
        LeaseEvidence {
            intervals,
            complete: self.journal.evicted_through() == 0,
        }
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
        let wire = self.intern_resource_wire(terminal);
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

    /// The hub consumer holding the satellite lease (L1 §9.1), if any.
    #[must_use]
    pub fn satellite_lease_holder(
        &self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) -> Option<ClientId> {
        self.leases.satellite_holder(host, terminal)
    }

    /// Record `client` as the satellite lease holder after the satellite
    /// acked, returning an evicted different holder to notify.
    pub(crate) fn set_satellite_lease(
        &mut self,
        host: phux_protocol::ids::SatelliteHost,
        terminal: u32,
        client: ClientId,
        out_tx: mpsc::Sender<Outbound>,
    ) -> Option<SatelliteLease> {
        self.leases.set_satellite(host, terminal, client, out_tx)
    }

    /// Mirror a satellite `terminal_control` into the hub ledger (the
    /// satellite owns the timer). `is_end` is true for Released/Expired;
    /// stale ends are ignored (see `LeaseTable::mirror_satellite_lease_event`).
    pub fn mirror_satellite_lease_event(
        &mut self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
        is_end: bool,
        seq: Option<u64>,
    ) -> Option<ClientId> {
        self.leases
            .mirror_satellite_lease_event(host, terminal, is_end, seq)
    }

    /// Forget the lease-mirror ordering state of a satellite terminal that
    /// closed.
    pub fn forget_satellite_lease_terminal(
        &mut self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) {
        self.leases.forget_satellite_terminal(host, terminal);
    }

    /// Entries in the satellite lease-mirror ordering state.
    #[cfg(test)]
    pub(crate) fn satellite_lease_mirror_entries(&self) -> usize {
        self.leases.satellite_mirror_entries()
    }

    /// Mark an `ACQUIRE_INPUT` relay in flight; clear on failure, and the
    /// next mirrored event clears it on success.
    pub fn mark_satellite_lease_acquire_pending(
        &mut self,
        host: phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) {
        self.leases.mark_satellite_acquire_pending(host, terminal);
    }

    /// Clear the pending mark set by
    /// [`Self::mark_satellite_lease_acquire_pending`].
    pub fn clear_satellite_lease_acquire_pending(
        &mut self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) {
        self.leases.clear_satellite_acquire_pending(host, terminal);
    }

    /// Release the hub-side satellite lease over `(host, terminal)` if
    /// `client` holds it. Returns `true` when an entry was removed.
    pub fn release_satellite_lease(
        &mut self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
        client: ClientId,
    ) -> bool {
        self.leases.release_satellite(host, terminal, client)
    }

    /// Every satellite lease `client` holds (for disconnect relays).
    #[must_use]
    pub fn satellite_leases_held_by(
        &self,
        client: ClientId,
    ) -> Vec<(phux_protocol::ids::SatelliteHost, u32)> {
        self.leases.satellite_held_by(client)
    }

    // -- satellite proxy attach registrations -----------------------------

    /// Whether `client` holds a proxied attach over `(host, terminal)`; every
    /// relayed frame is gated on it.
    #[must_use]
    pub fn has_satellite_proxy_attach(
        &self,
        client: ClientId,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) -> bool {
        self.leases
            .has_satellite_proxy_attach(client, host, terminal)
    }

    /// Record that `client` now proxies `terminal` on `host`.
    pub fn register_satellite_proxy_attach(
        &mut self,
        client: ClientId,
        host: phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) {
        self.leases
            .register_satellite_proxy_attach(client, host, terminal);
    }

    /// Drop one proxy registration. Idempotent.
    pub fn unregister_satellite_proxy_attach(
        &mut self,
        client: ClientId,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) {
        self.leases
            .unregister_satellite_proxy_attach(client, host, terminal);
    }
}

/// One lease epoch as the journal recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseInterval {
    /// The epoch.
    pub epoch: LeaseEpoch,
    /// Who held the lease through it.
    pub holder: Option<ActorRef>,
    /// When it opened, Unix milliseconds.
    pub from_ms: u64,
    /// When the next epoch opened; `None` while it is current.
    pub until_ms: Option<u64>,
}

/// Who was driving a resource over an interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseEvidence {
    /// The held epochs overlapping the interval, oldest first.
    pub intervals: Vec<LeaseInterval>,
    /// `false` once the journal has evicted anything, so an earlier epoch
    /// may be missing from the answer.
    pub complete: bool,
}

#[cfg(test)]
mod tests;
