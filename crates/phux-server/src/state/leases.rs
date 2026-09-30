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

    /// Whether another client's lease blocks `client`'s input (ADR-0033).
    #[must_use]
    pub fn input_blocked(&self, terminal: ResourceId, client: ClientId) -> bool {
        self.leases.blocked(terminal, client)
    }

    /// Grant `pane`'s input lease to `client` (ADR-0033), returning the prior
    /// holder if the lease was already held (a `Seize` preemption).
    pub fn set_input_lease(&mut self, terminal: ResourceId, client: ClientId) -> Option<ClientId> {
        self.leases.acquire(terminal, client)
    }

    /// Release the lease if `client` holds it; `true` if released.
    pub fn release_input_lease(&mut self, terminal: ResourceId, client: ClientId) -> bool {
        self.leases.release(terminal, client)
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
