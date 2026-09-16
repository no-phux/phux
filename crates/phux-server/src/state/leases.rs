use phux_core::ids::ResourceId;
use tokio::sync::mpsc;

use super::{ClientId, Outbound, SatelliteLease, ServerState};

impl ServerState {
    /// The client currently holding `pane`'s input lease (ADR-0033), or
    /// `None` if the pane is `Open`.
    #[must_use]
    pub fn input_lease_holder(&self, terminal: ResourceId) -> Option<ClientId> {
        self.leases.holder(terminal)
    }

    /// Whether `client`'s input to `pane` is blocked by another client's
    /// lease (ADR-0033). `false` when the pane is `Open` or `client` is the
    /// holder. The gate calls this before forwarding input to the actor.
    #[must_use]
    pub fn input_blocked(&self, terminal: ResourceId, client: ClientId) -> bool {
        self.leases.blocked(terminal, client)
    }

    /// Grant `pane`'s input lease to `client` (ADR-0033), returning the prior
    /// holder if the lease was already held (a `Seize` preemption).
    pub fn set_input_lease(&mut self, terminal: ResourceId, client: ClientId) -> Option<ClientId> {
        self.leases.acquire(terminal, client)
    }

    /// Release `pane`'s input lease if `client` holds it (ADR-0033). Returns
    /// `true` if a lease was actually released. A no-op (returns `false`) if
    /// the pane is `Open` or held by someone else.
    pub fn release_input_lease(&mut self, terminal: ResourceId, client: ClientId) -> bool {
        self.leases.release(terminal, client)
    }

    /// Every pane whose input lease `client` currently holds (ADR-0033). The
    /// runtime reads this at disconnect time to broadcast `Released` events
    /// before [`Self::detach`] clears the leases.
    #[must_use]
    pub fn leases_held_by(&self, client: ClientId) -> Vec<ResourceId> {
        self.leases.held_by(client)
    }

    /// Invalidate `terminal`'s current expiry generation and, when
    /// `scheduled` is true, record a fresh one (ADR-0033's `ttl_ms`, this
    /// lane). The caller pairs a `Some` return with the `ttl_ms` it asked
    /// for and arms a timer under that generation; a stale timer's wake
    /// checks it with [`Self::input_lease_expiry_is_current`]. Called from
    /// the same lock scope as [`Self::set_input_lease`] /
    /// [`Self::release_input_lease`] so the lease and its TTL tracking
    /// never observe each other mid-update.
    pub fn refresh_input_lease_expiry(
        &mut self,
        terminal: ResourceId,
        scheduled: bool,
    ) -> Option<u64> {
        self.leases.refresh_expiry(terminal, scheduled)
    }

    /// Whether `generation` is still the expiry generation on file for
    /// `terminal` — the check a woken TTL timer makes before treating
    /// itself as the one that gets to expire the lease.
    #[must_use]
    pub fn input_lease_expiry_is_current(&self, terminal: ResourceId, generation: u64) -> bool {
        self.leases.expiry_is_current(terminal, generation)
    }

    /// Record `terminal`'s just-spawned TTL timer's abort handle, provided
    /// `generation` is still current. `false` means the caller should
    /// abort the handle itself: the timer was already superseded between
    /// minting `generation` and getting here.
    #[must_use]
    pub fn attach_input_lease_expiry_abort(
        &mut self,
        terminal: ResourceId,
        generation: u64,
        abort: tokio::task::AbortHandle,
    ) -> bool {
        self.leases.attach_expiry_abort(terminal, generation, abort)
    }

    /// Clear `terminal`'s expiry tracking without aborting its timer task
    /// — for the timer's own legitimate firing; see
    /// `state::lease_table::LeaseTable::clear_expiry` for why that one
    /// case must not go through the aborting `refresh_input_lease_expiry`.
    pub fn clear_input_lease_expiry(&mut self, terminal: ResourceId) {
        self.leases.clear_expiry(terminal);
    }

    /// A clone of the live-TTL-timer-task counter, for the
    /// `runtime::commands` regression test proving repeated re-arms do not
    /// accumulate sleeping tasks.
    #[must_use]
    pub fn input_lease_expiry_task_counter(
        &self,
    ) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        self.leases.expiry_task_counter()
    }

    /// The hub consumer currently holding the input lease over satellite
    /// terminal `(host, id)` (phux-v45.7, L1 §9.1), or `None` when free.
    /// See the `LeaseTable::satellite` field doc for why this ledger exists.
    #[must_use]
    pub fn satellite_lease_holder(
        &self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) -> Option<ClientId> {
        self.leases.satellite_holder(host, terminal)
    }

    /// Record `client` (with its outbound mailbox `out_tx`) as the
    /// hub-side holder of the satellite lease, after the satellite acked
    /// the relayed `ACQUIRE_INPUT`.
    ///
    /// Returns the **evicted** prior lease when this acquire preempted a
    /// *different* hub consumer (a SEIZE takeover, phux-v45.13): the caller
    /// notifies that holder it lost the wheel. A re-acquire by the same
    /// holder (idempotent cooperative acquire) or a grant over a free lease
    /// returns `None` — nobody was evicted.
    pub(crate) fn set_satellite_lease(
        &mut self,
        host: phux_protocol::ids::SatelliteHost,
        terminal: u32,
        client: ClientId,
        out_tx: mpsc::Sender<Outbound>,
    ) -> Option<SatelliteLease> {
        self.leases.set_satellite(host, terminal, client, out_tx)
    }

    /// Mirror one satellite-originated `terminal_control` transition for
    /// `(host, terminal)` into the hub's own ledger (ADR-0033's `ttl_ms`,
    /// this lane): the satellite owns the TTL timer and is the source of
    /// truth for whether its lease is free, so — unlike
    /// [`Self::release_satellite_lease`] — this does not check who the hub
    /// thinks holds it. `is_end` is `true` for Released/Expired, `false`
    /// for Acquired/Seized. Returns the evicted holder when a
    /// Released/Expired was actually applied.
    ///
    /// Ignores a Released/Expired that races an in-flight
    /// `ACQUIRE_INPUT` relay's reply, or that carries a `seq` no newer
    /// than the last Acquired/Seized mirrored — see
    /// `state::lease_table::LeaseTable::mirror_satellite_lease_event`'s
    /// doc for the ordering hazard this closes and why it is bounded
    /// rather than provably complete.
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

    /// Mark `(host, terminal)`'s lease pending: an `ACQUIRE_INPUT` relay is
    /// in flight for it. Call before awaiting the relay's reply. On
    /// failure, pair with [`Self::clear_satellite_lease_acquire_pending`];
    /// on success, leave it — [`Self::mirror_satellite_lease_event`]
    /// clears it on the next event for this terminal instead.
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

    /// Every satellite lease `client` currently holds. Read at disconnect
    /// time so the runtime can relay a detached `RELEASE_INPUT` per entry
    /// before [`Self::detach`] clears the ledger.
    #[must_use]
    pub fn satellite_leases_held_by(
        &self,
        client: ClientId,
    ) -> Vec<(phux_protocol::ids::SatelliteHost, u32)> {
        self.leases.satellite_held_by(client)
    }

    // -- satellite proxy attach registrations -----------------------------

    /// Whether `client` holds a proxied `ATTACH_RESOURCE` over `terminal` on
    /// `host`.
    ///
    /// The hub relays opaque reply and input frames on a consumer's behalf and
    /// cannot re-derive entitlement from the frame alone, so every relayed
    /// frame is gated on an exact registration made here at attach time.
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
