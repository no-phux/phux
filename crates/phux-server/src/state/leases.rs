use phux_core::ids::TerminalId;
use tokio::sync::mpsc;

use super::{ClientId, Outbound, SatelliteLease, ServerState};

impl ServerState {
    /// The client currently holding `pane`'s input lease (ADR-0033), or
    /// `None` if the pane is `Open`.
    #[must_use]
    pub fn input_lease_holder(&self, terminal: TerminalId) -> Option<ClientId> {
        self.leases.holder(terminal)
    }

    /// Whether `client`'s input to `pane` is blocked by another client's
    /// lease (ADR-0033). `false` when the pane is `Open` or `client` is the
    /// holder. The gate calls this before forwarding input to the actor.
    #[must_use]
    pub fn input_blocked(&self, terminal: TerminalId, client: ClientId) -> bool {
        self.leases.blocked(terminal, client)
    }

    /// Grant `pane`'s input lease to `client` (ADR-0033), returning the prior
    /// holder if the lease was already held (a `Seize` preemption).
    pub fn set_input_lease(&mut self, terminal: TerminalId, client: ClientId) -> Option<ClientId> {
        self.leases.acquire(terminal, client)
    }

    /// Release `pane`'s input lease if `client` holds it (ADR-0033). Returns
    /// `true` if a lease was actually released. A no-op (returns `false`) if
    /// the pane is `Open` or held by someone else.
    pub fn release_input_lease(&mut self, terminal: TerminalId, client: ClientId) -> bool {
        self.leases.release(terminal, client)
    }

    /// Every pane whose input lease `client` currently holds (ADR-0033). The
    /// runtime reads this at disconnect time to broadcast `Released` events
    /// before [`Self::detach`] clears the leases.
    #[must_use]
    pub fn leases_held_by(&self, client: ClientId) -> Vec<TerminalId> {
        self.leases.held_by(client)
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

    /// Whether `client` holds a proxied `ATTACH_TERMINAL` over `terminal` on
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
        self.leases.has_satellite_proxy_attach(client, host, terminal)
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
