use phux_core::ids::TerminalId;
use tokio::sync::mpsc;

use super::{ClientId, Outbound, SatelliteLease, ServerState};

impl ServerState {
    /// The client currently holding `pane`'s input lease (ADR-0033), or
    /// `None` if the pane is `Open`.
    #[must_use]
    pub fn input_lease_holder(&self, terminal: TerminalId) -> Option<ClientId> {
        self.input_leases.get(&terminal).copied()
    }

    /// Whether `client`'s input to `pane` is blocked by another client's
    /// lease (ADR-0033). `false` when the pane is `Open` or `client` is the
    /// holder. The gate calls this before forwarding input to the actor.
    #[must_use]
    pub fn input_blocked(&self, terminal: TerminalId, client: ClientId) -> bool {
        self.input_leases
            .get(&terminal)
            .is_some_and(|holder| *holder != client)
    }

    /// Grant `pane`'s input lease to `client` (ADR-0033), returning the prior
    /// holder if the lease was already held (a `Seize` preemption).
    pub fn set_input_lease(&mut self, terminal: TerminalId, client: ClientId) -> Option<ClientId> {
        self.input_leases.insert(terminal, client)
    }

    /// Release `pane`'s input lease if `client` holds it (ADR-0033). Returns
    /// `true` if a lease was actually released. A no-op (returns `false`) if
    /// the pane is `Open` or held by someone else.
    pub fn release_input_lease(&mut self, terminal: TerminalId, client: ClientId) -> bool {
        if self.input_leases.get(&terminal) == Some(&client) {
            self.input_leases.remove(&terminal);
            true
        } else {
            false
        }
    }

    /// Every pane whose input lease `client` currently holds (ADR-0033). The
    /// runtime reads this at disconnect time to broadcast `Released` events
    /// before [`Self::detach`] clears the leases.
    #[must_use]
    pub fn leases_held_by(&self, client: ClientId) -> Vec<TerminalId> {
        self.input_leases
            .iter()
            .filter_map(|(pane, holder)| (*holder == client).then_some(*pane))
            .collect()
    }

    /// The hub consumer currently holding the input lease over satellite
    /// terminal `(host, id)` (phux-v45.7, L1 §9.1), or `None` when free.
    /// See the `satellite_leases` field doc for why this ledger exists.
    #[must_use]
    pub fn satellite_lease_holder(
        &self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) -> Option<ClientId> {
        self.satellite_leases
            .get(&(host.clone(), terminal))
            .map(|lease| lease.holder)
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
        let prior = self.satellite_leases.insert(
            (host, terminal),
            SatelliteLease {
                holder: client,
                out_tx,
            },
        );
        prior.filter(|lease| lease.holder != client)
    }

    /// Release the hub-side satellite lease over `(host, terminal)` if
    /// `client` holds it. Returns `true` when an entry was removed.
    pub fn release_satellite_lease(
        &mut self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
        client: ClientId,
    ) -> bool {
        let key = (host.clone(), terminal);
        if self.satellite_leases.get(&key).map(|lease| lease.holder) == Some(client) {
            self.satellite_leases.remove(&key);
            true
        } else {
            false
        }
    }

    /// Every satellite lease `client` currently holds. Read at disconnect
    /// time so the runtime can relay a detached `RELEASE_INPUT` per entry
    /// before [`Self::detach`] clears the ledger.
    #[must_use]
    pub fn satellite_leases_held_by(
        &self,
        client: ClientId,
    ) -> Vec<(phux_protocol::ids::SatelliteHost, u32)> {
        self.satellite_leases
            .iter()
            .filter(|(_, lease)| lease.holder == client)
            .map(|(key, _)| key.clone())
            .collect()
    }
}
