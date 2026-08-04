//! The input-lease ledgers (ADR-0033, "take the wheel"): who currently
//! owns keystroke delivery to a local pane, and — on a federation hub —
//! which hub consumer owns it over a satellite pane (phux-v45.7).
//!
//! Two maps that were flat on [`super::ServerState`] live here because
//! they share one lifetime — a *holder's* connection. An entry appears
//! when a client's `ACQUIRE_INPUT` is granted and both disappear together
//! in `ServerState::detach`, which is exactly the invariant that must not
//! drift: a disconnect that strands either ledger leaves a pane nobody can
//! type into. That teardown is now one call
//! ([`LeaseTable::release_all_for`]) instead of two open-coded `retain`s in
//! `state::client`.
//!
//! # Ownership boundary
//!
//! This type owns the *ledgers*, not the protocol around them. Everything
//! that decides whether a lease may change hands — the relay round trip to
//! the satellite, the `TerminalControl(Seized)` notification to an evicted
//! holder, the `Released` broadcast at disconnect — stays in
//! `runtime::commands` / `runtime::client` and calls in through the
//! delegating accessors on `ServerState` (see `state::leases`).
//!
//! Both fields are private: unlike `state::config` and
//! `state::client_table`, nothing on `ServerState` needs to borrow-split a
//! lease map against another field, so every read and write goes through a
//! method here.
//!
//! Nothing here is `async` and nothing awaits, so the state lock can never
//! be held across a suspension point through this type.
//!
//! The struct and every method are `pub(super)`: the accessors the runtime
//! calls stay on `ServerState`, so the crate's public surface is unchanged
//! and both maps stay exactly as unreachable from outside `state` as they
//! were as private fields.

use std::collections::{BTreeMap, HashMap, HashSet};

use phux_core::ids::TerminalId;
use tokio::sync::mpsc;

use super::client::ClientId;
use super::input_log::Outbound;

/// One hub-side satellite input lease (phux-v45.7, phux-v45.13).
///
/// Records which hub consumer holds the relayed ADR-0033 lease over a
/// satellite terminal **and** that consumer's outbound mailbox. The
/// mailbox is what lets a SEIZE takeover by a *different* hub consumer
/// notify the evicted prior holder directly (a hub-synthesized
/// `TerminalControl(Seized)` event, mirroring the local takeover
/// broadcast) — the satellite cannot do it, because every hub consumer
/// reaches it through the link's single client identity, so its own lease
/// change reads as a same-identity re-acquire.
#[derive(Debug, Clone)]
pub(crate) struct SatelliteLease {
    /// The hub consumer that holds the lease.
    pub(crate) holder: ClientId,
    /// The holder's outbound mailbox, for the eviction notification.
    pub(crate) out_tx: mpsc::Sender<Outbound>,
}

/// Both input-lease ledgers the server owns: the local per-pane leases and
/// the hub-side per-satellite-pane leases.
///
/// Held as a single field on [`super::ServerState`]. Not thread-safe on
/// its own; the surrounding `Mutex<ServerState>` provides synchronization.
#[derive(Debug)]
pub(super) struct LeaseTable {
    /// Per-pane input lease (ADR-0033). When a pane has an entry, only that
    /// `ClientId`'s input reaches the PTY; everyone else's `INPUT_*` /
    /// `ROUTE_INPUT` is dropped at the gate (still acked, per the
    /// fire-and-forget input invariant). Absent = `Open`: any subscriber's
    /// input passes (the back-compat default). Released automatically when
    /// the holder detaches or its connection drops.
    input: HashMap<TerminalId, ClientId>,
    /// Hub-side ledger of which **hub consumer** owns the input lease over
    /// a satellite terminal (phux-v45.7). All hub consumers share the
    /// link's single client identity on the satellite, so the satellite's
    /// own lease map cannot tell them apart: without this ledger, consumer
    /// A's `ACQUIRE_INPUT` over a satellite terminal would not exclude
    /// consumer B's relayed input, and B's `RELEASE_INPUT` would release
    /// A's lease. The hub therefore gates relayed `ACQUIRE_INPUT` /
    /// `RELEASE_INPUT` / `ROUTE_INPUT` / `INPUT_*` on this map *before*
    /// forwarding, and the satellite-side lease (held by the link
    /// identity) keeps excluding the satellite's own local clients.
    /// Entries are keyed `(host, satellite-local id)` and cleared when the
    /// holder detaches (with a detached `RELEASE_INPUT` relayed so the
    /// satellite-side lease follows). Each entry carries the holder's
    /// outbound mailbox so a SEIZE takeover by another hub consumer can
    /// notify the evicted prior holder directly (phux-v45.13) — the
    /// satellite cannot, since it sees only the shared link identity. See
    /// L1 §9.1.
    satellite: BTreeMap<(phux_protocol::ids::SatelliteHost, u32), SatelliteLease>,
    /// Successful satellite `ATTACH_TERMINAL` proxy ownership, mirrored at the
    /// hub authority boundary.
    ///
    /// The hub relays opaque reply and input frames on behalf of a consumer,
    /// and cannot re-derive from the frame alone whether that consumer is
    /// entitled to the satellite terminal it names. A frame must match one of
    /// these exact `(client, host, terminal)` registrations before the hub
    /// forwards it, so a consumer cannot address a satellite pane it never
    /// attached to.
    satellite_proxy_attaches: HashSet<(ClientId, phux_protocol::ids::SatelliteHost, u32)>,
}

impl Default for LeaseTable {
    fn default() -> Self {
        Self::new()
    }
}

impl LeaseTable {
    /// Build an empty ledger pair — every pane starts `Open`.
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            input: HashMap::new(),
            satellite: BTreeMap::new(),
            satellite_proxy_attaches: HashSet::new(),
        }
    }

    // -- satellite proxy attach registrations -----------------------------

    /// Whether `client` holds a proxied `ATTACH_TERMINAL` over `terminal` on
    /// `host`. The gate every relayed reply/input frame passes.
    pub(super) fn has_satellite_proxy_attach(
        &self,
        client: ClientId,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) -> bool {
        self.satellite_proxy_attaches
            .contains(&(client, host.clone(), terminal))
    }

    /// Record that `client` now proxies `terminal` on `host`.
    pub(super) fn register_satellite_proxy_attach(
        &mut self,
        client: ClientId,
        host: phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) {
        self.satellite_proxy_attaches
            .insert((client, host, terminal));
    }

    /// Drop one proxy registration. Idempotent.
    pub(super) fn unregister_satellite_proxy_attach(
        &mut self,
        client: ClientId,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) {
        self.satellite_proxy_attaches
            .remove(&(client, host.clone(), terminal));
    }

    // -- local pane leases ----------------------------------------------

    /// The client currently holding `terminal`'s input lease (ADR-0033), or
    /// `None` if the pane is `Open`.
    #[must_use]
    pub(super) fn holder(&self, terminal: TerminalId) -> Option<ClientId> {
        self.input.get(&terminal).copied()
    }

    /// Whether `client`'s input to `terminal` is blocked by another
    /// client's lease. `false` when the pane is `Open` or `client` is the
    /// holder.
    #[must_use]
    pub(super) fn blocked(&self, terminal: TerminalId, client: ClientId) -> bool {
        self.input
            .get(&terminal)
            .is_some_and(|holder| *holder != client)
    }

    /// Grant `terminal`'s input lease to `client`, returning the prior
    /// holder if the lease was already held (a `Seize` preemption).
    pub(super) fn acquire(&mut self, terminal: TerminalId, client: ClientId) -> Option<ClientId> {
        self.input.insert(terminal, client)
    }

    /// Release `terminal`'s input lease if `client` holds it. Returns
    /// `true` if a lease was actually released. A no-op (returns `false`)
    /// if the pane is `Open` or held by someone else.
    pub(super) fn release(&mut self, terminal: TerminalId, client: ClientId) -> bool {
        if self.input.get(&terminal) == Some(&client) {
            self.input.remove(&terminal);
            true
        } else {
            false
        }
    }

    /// Every pane whose input lease `client` currently holds.
    #[must_use]
    pub(super) fn held_by(&self, client: ClientId) -> Vec<TerminalId> {
        self.input
            .iter()
            .filter_map(|(pane, holder)| (*holder == client).then_some(*pane))
            .collect()
    }

    // -- hub-side satellite leases (phux-v45.7) -------------------------

    /// The hub consumer currently holding the input lease over satellite
    /// terminal `(host, terminal)` (L1 §9.1), or `None` when free.
    #[must_use]
    pub(super) fn satellite_holder(
        &self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) -> Option<ClientId> {
        self.satellite
            .get(&(host.clone(), terminal))
            .map(|lease| lease.holder)
    }

    /// Record `client` (with its outbound mailbox `out_tx`) as the
    /// hub-side holder of the satellite lease.
    ///
    /// Returns the **evicted** prior lease when this acquire preempted a
    /// *different* hub consumer (a SEIZE takeover, phux-v45.13). A
    /// re-acquire by the same holder (idempotent cooperative acquire) or a
    /// grant over a free lease returns `None` — nobody was evicted.
    pub(super) fn set_satellite(
        &mut self,
        host: phux_protocol::ids::SatelliteHost,
        terminal: u32,
        client: ClientId,
        out_tx: mpsc::Sender<Outbound>,
    ) -> Option<SatelliteLease> {
        let prior = self.satellite.insert(
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
    pub(super) fn release_satellite(
        &mut self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
        client: ClientId,
    ) -> bool {
        let key = (host.clone(), terminal);
        if self.satellite.get(&key).map(|lease| lease.holder) == Some(client) {
            self.satellite.remove(&key);
            true
        } else {
            false
        }
    }

    /// Every satellite lease `client` currently holds.
    #[must_use]
    pub(super) fn satellite_held_by(
        &self,
        client: ClientId,
    ) -> Vec<(phux_protocol::ids::SatelliteHost, u32)> {
        self.satellite
            .iter()
            .filter(|(_, lease)| lease.holder == client)
            .map(|(key, _)| key.clone())
            .collect()
    }

    // -- disconnect teardown --------------------------------------------

    /// Drop every lease `client` holds, local and hub-side, in one step.
    ///
    /// Called from [`super::ServerState::detach`] so a disconnect never
    /// strands the wheel. The runtime broadcasts the `Released` events (via
    /// [`super::ServerState::leases_held_by`]) and relays the detached
    /// `RELEASE_INPUT` per satellite entry (via
    /// [`super::ServerState::satellite_leases_held_by`]) *before* calling
    /// detach; this clears both ledgers regardless of those paths running.
    pub(super) fn release_all_for(&mut self, client: ClientId) {
        self.input.retain(|_, holder| *holder != client);
        self.satellite.retain(|_, lease| lease.holder != client);
    }
}
