//! Hub-only state (ADR-0007): the satellite table, relays, and link
//! supervisors; what each satellite advertised; the satellite spawn ledger
//! (ADR-0109); and the hub-side satellite input leases.
//!
//! The table, relays, and links are installed together at startup and are
//! `None` exactly when the server is not a hub; a registry reload swaps the
//! table in place. The federation protocol lives in the runtime and
//! [`crate::hub`]. Satellite-side uplinks stay on [`super::ServerState`].
//! Everything here is sync.

use std::collections::{BTreeMap, HashMap, HashSet};

use phux_protocol::ids::SatelliteHost;
use tokio::sync::mpsc;

use super::client::ClientId;
use crate::mailbox::Outbound;

mod spawns;

/// One hub-side satellite input lease: the holding hub consumer and its
/// mailbox, so a SEIZE by another consumer can notify the evicted holder
/// (the satellite sees only the link's single identity and cannot).
#[derive(Debug, Clone)]
pub(crate) struct SatelliteLease {
    /// The hub consumer that holds the lease.
    pub(crate) holder: ClientId,
    /// The holder's outbound mailbox, for the eviction notification.
    pub(crate) out_tx: mpsc::Sender<Outbound>,
}

/// A satellite terminal, by host and satellite-local id.
type SatelliteTerminal = (SatelliteHost, u32);

/// Every hub-only table; empty (and every handle `None`) off-hub.
#[derive(Debug, Default)]
pub(crate) struct Federation {
    /// Validated satellite table.
    table: Option<crate::hub::HubTable>,
    /// Per-satellite relay handles, used to route `ResourceId::Satellite`.
    relays: Option<crate::hub::relay::HubRelays>,
    /// The running link supervisors, so a reload can start and stop links.
    links: Option<crate::hub::link::HubLinks>,
    /// Where a reload re-reads `[[satellites]]`; `None` disables reload.
    source: Option<crate::hub::SatelliteSource>,
    /// Parent of link supervisors started by a later promotion. `None` until
    /// the runtime arms reload.
    cancel: Option<tokio_util::sync::CancellationToken>,
    /// What each satellite advertised on its current link (ADR-0127), set by
    /// the link's relay session when it negotiates.
    satellite_features: Vec<(SatelliteHost, phux_protocol::caps::ServerFeatureSet)>,
    /// Where the QUIC listener hands satellites that dial in (ADR-0182); it
    /// accepts none until an `inbound://` link registers.
    adopter: crate::hub::adopt::SatelliteAdopter,
    /// Who spawned each satellite resource and whether another consumer
    /// attached it since (ADR-0109).
    spawns: spawns::SatelliteSpawnLedger,
    /// Hub-side lease per satellite terminal. All hub consumers
    /// share one identity on the satellite, so the hub gates relayed input
    /// and lease commands here before forwarding. Carries the holder's
    /// mailbox for SEIZE notifications.
    leases: BTreeMap<SatelliteTerminal, SatelliteLease>,
    /// Proxied satellite `ATTACH_RESOURCE`s. A relayed frame must match an
    /// exact `(client, host, terminal)` entry, so a consumer cannot address
    /// a satellite pane it never attached.
    proxy_attaches: HashSet<(ClientId, SatelliteHost, u32)>,
    /// Satellite terminals with an `ACQUIRE_INPUT` relay in flight; see
    /// [`Self::mirror_satellite_lease_event`].
    acquire_pending: HashSet<SatelliteTerminal>,
    /// The satellite's `seq` of the last mirrored Acquired/Seized per
    /// terminal, when the link carries stamps.
    last_mirrored_seq: HashMap<SatelliteTerminal, u64>,
}

// -- hub handles ---------------------------------------------------------
impl Federation {
    /// Install the validated satellite table (hub mode, at startup).
    pub(crate) fn set_table(&mut self, table: crate::hub::HubTable) {
        self.table = Some(table);
    }

    /// The hub satellite table; `None` on a non-hub server.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn table(&self) -> Option<&crate::hub::HubTable> {
        self.table.as_ref()
    }

    /// Whether this server is a hub: a satellite table is installed.
    #[must_use]
    pub(crate) const fn is_hub(&self) -> bool {
        self.table.is_some()
    }

    /// Install the shared per-satellite frame-relay registry (hub startup).
    pub(crate) fn set_relays(&mut self, relays: crate::hub::relay::HubRelays) {
        self.relays = Some(relays);
    }

    /// Install the running link supervisors and the registry source a live
    /// reload re-reads (hub startup).
    pub(crate) fn set_links(
        &mut self,
        links: crate::hub::link::HubLinks,
        source: Option<crate::hub::SatelliteSource>,
    ) {
        self.links = Some(links);
        self.source = source;
    }

    /// The registry source [`crate::hub::reload_satellites`] re-reads;
    /// `None` when the embedder supplied none.
    #[must_use]
    pub(crate) fn source(&self) -> Option<crate::hub::SatelliteSource> {
        self.source.clone()
    }

    /// Arm reload before any link exists: remember the registry reader and
    /// the token that will parent any link started later, so a later
    /// doorbell can promote this server into a hub without restarting panes.
    pub(crate) fn arm_reload(
        &mut self,
        source: Option<crate::hub::SatelliteSource>,
        cancel: tokio_util::sync::CancellationToken,
    ) {
        if source.is_some() {
            self.source = source;
        }
        self.cancel = Some(cancel);
    }

    /// Where the QUIC listener hands satellites that dial in (ADR-0182); the
    /// adopter every link supervisor and the listener share.
    #[must_use]
    pub(crate) fn adopter(&self) -> crate::hub::adopt::SatelliteAdopter {
        self.adopter.clone()
    }

    /// Whether satellite link supervisors are installed.
    #[must_use]
    pub(crate) const fn has_links(&self) -> bool {
        self.links.is_some()
    }

    /// Install empty relays and supervisors when a doorbell is about to add
    /// the first satellites, so the next table swap can dial. `false` when
    /// reload was not armed with a token. Existing supervisors stay.
    pub(crate) fn ensure_links(
        &mut self,
        ssh_program: std::ffi::OsString,
        bootstrap: phux_protocol::caps::BootstrapCapabilities,
    ) -> bool {
        if self.links.is_some() {
            return true;
        }
        let Some(cancel) = self.cancel.clone() else {
            return false;
        };
        let Ok(empty) = crate::hub::HubTable::from_registry(&[]) else {
            return false;
        };
        let relays = crate::hub::relay::HubRelays::default();
        let links = crate::hub::link::HubLinks::new(
            relays.clone(),
            &cancel,
            ssh_program,
            self.adopter.clone(),
            bootstrap,
        );
        self.relays = Some(relays);
        if self.table.is_none() {
            self.table = Some(empty);
        }
        self.links = Some(links);
        true
    }

    /// Swap in a reloaded satellite table, starting, stopping, or redialing
    /// only the links that differ. `journal` is this state's own handle,
    /// cloned into each new link. Off-hub nothing changes.
    pub(crate) fn replace_table(
        &mut self,
        next: crate::hub::HubTable,
        journal: &super::SharedState,
    ) -> crate::hub::TableDiff {
        let (Some(table), Some(links)) = (self.table.as_mut(), self.links.as_mut()) else {
            return crate::hub::TableDiff::default();
        };
        let diff = table.diff(&next);
        // A changed endpoint may be another server: forget what the old
        // link advertised until the new one negotiates (ADR-0127).
        for host in diff.removed.iter().chain(&diff.changed) {
            links.stop(host);
            self.satellite_features.retain(|(known, _)| known != host);
        }
        for host in diff.added.iter().chain(&diff.changed) {
            if let Some(entry) = next.get(host) {
                links.start(host, entry, journal);
            }
        }
        *table = next;
        diff
    }

    /// The relay for `host`; `None` off-hub or for an unknown host.
    #[must_use]
    pub(crate) fn relay(&self, host: &SatelliteHost) -> Option<crate::hub::relay::RelayHandle> {
        self.relays.as_ref().and_then(|relays| relays.get(host))
    }

    /// The relay for `host`, or why a request for it is refused: the host is
    /// not in this hub's registry, or this server is not a hub.
    pub(crate) fn relay_route(
        &self,
        host: &SatelliteHost,
    ) -> Result<crate::hub::relay::RelayHandle, String> {
        if let Some(relay) = self.relay(host) {
            return Ok(relay);
        }
        if self.is_hub() {
            return Err(format!("no satellite named {host} in this hub's registry"));
        }
        Err(format!(
            "this server is not a federation hub; it has no route to satellite {host}"
        ))
    }

    /// Every satellite relay handle (detach fan-out); empty off-hub.
    #[must_use]
    pub(crate) fn relays_all(&self) -> Vec<crate::hub::relay::RelayHandle> {
        self.relays
            .as_ref()
            .map(crate::hub::relay::HubRelays::all)
            .unwrap_or_default()
    }

    /// Record what `host` advertised on its newest link (ADR-0127), from the
    /// link's relay session when it negotiates, replacing any earlier
    /// link's.
    pub(crate) fn set_satellite_features(
        &mut self,
        host: SatelliteHost,
        features: phux_protocol::caps::ServerFeatureSet,
    ) {
        self.satellite_features.retain(|(known, _)| *known != host);
        self.satellite_features.push((host, features));
    }

    /// Whether `host` advertised `feature` on its current link (ADR-0127);
    /// `false` for a host with no negotiated link.
    #[must_use]
    pub(crate) fn satellite_advertises(
        &self,
        host: &SatelliteHost,
        feature: phux_protocol::caps::ServerFeature,
    ) -> bool {
        self.satellite_features
            .iter()
            .find(|(known, _)| known == host)
            .is_some_and(|(_, features)| features.contains(feature))
    }
}

// -- satellite spawn ledger (ADR-0109) ------------------------------------
impl Federation {
    /// Remember that hub consumer `spawner` asked for `host`'s resource `id`,
    /// bound to `instance` when the spawn asked for binding.
    pub(crate) fn record_satellite_spawn(
        &mut self,
        host: SatelliteHost,
        id: u32,
        instance: Option<phux_protocol::ids::ServerInstance>,
        spawner: ClientId,
    ) {
        self.spawns.record_spawn(host, id, instance, spawner);
    }

    /// Note that hub consumer `client` is attaching or using `host`'s
    /// resource `id`.
    pub(crate) fn note_satellite_use(&mut self, host: &SatelliteHost, id: u32, client: ClientId) {
        self.spawns.note_use(host, id, client);
    }

    /// Whether this hub can vouch that `host`'s `id` is unattached since
    /// spawn under `instance`.
    #[must_use]
    pub(crate) fn vouches_unattached(
        &self,
        host: &SatelliteHost,
        id: u32,
        instance: phux_protocol::ids::ServerInstance,
    ) -> bool {
        self.spawns.vouches_unattached(host, id, instance)
    }
}

// -- satellite proxy attach registrations ---------------------------------
impl Federation {
    /// Whether `client` holds a proxied attach over `(host, terminal)`;
    /// every relayed frame is gated on it.
    #[must_use]
    pub(crate) fn has_satellite_proxy_attach(
        &self,
        client: ClientId,
        host: &SatelliteHost,
        terminal: u32,
    ) -> bool {
        self.proxy_attaches
            .contains(&(client, host.clone(), terminal))
    }

    /// Record that `client` now proxies `terminal` on `host`.
    pub(crate) fn register_satellite_proxy_attach(
        &mut self,
        client: ClientId,
        host: SatelliteHost,
        terminal: u32,
    ) {
        self.proxy_attaches.insert((client, host, terminal));
    }

    /// Drop one proxy registration. Idempotent.
    pub(crate) fn unregister_satellite_proxy_attach(
        &mut self,
        client: ClientId,
        host: &SatelliteHost,
        terminal: u32,
    ) {
        self.proxy_attaches
            .remove(&(client, host.clone(), terminal));
    }
}

// -- hub-side satellite leases ----------------------------------
impl Federation {
    /// The hub consumer holding the satellite lease, if any.
    #[must_use]
    pub(crate) fn satellite_lease_holder(
        &self,
        host: &SatelliteHost,
        terminal: u32,
    ) -> Option<ClientId> {
        self.leases
            .get(&(host.clone(), terminal))
            .map(|lease| lease.holder)
    }

    /// Record `client` (and mailbox) as the satellite lease holder after the
    /// satellite acked, returning the evicted lease to notify when a
    /// different consumer held it.
    pub(crate) fn set_satellite_lease(
        &mut self,
        host: SatelliteHost,
        terminal: u32,
        client: ClientId,
        out_tx: mpsc::Sender<Outbound>,
    ) -> Option<SatelliteLease> {
        let prior = self.leases.insert(
            (host, terminal),
            SatelliteLease {
                holder: client,
                out_tx,
            },
        );
        prior.filter(|lease| lease.holder != client)
    }

    /// Release the satellite lease over `(host, terminal)` if `client` holds
    /// it; `true` when an entry was removed.
    pub(crate) fn release_satellite_lease(
        &mut self,
        host: &SatelliteHost,
        terminal: u32,
        client: ClientId,
    ) -> bool {
        let key = (host.clone(), terminal);
        if self.leases.get(&key).map(|lease| lease.holder) != Some(client) {
            return false;
        }
        self.leases.remove(&key);
        true
    }

    /// Every satellite lease `client` holds (for disconnect relays).
    #[must_use]
    pub(crate) fn satellite_leases_held_by(&self, client: ClientId) -> Vec<SatelliteTerminal> {
        self.leases
            .iter()
            .filter(|(_, lease)| lease.holder == client)
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Drop every satellite lease `client` holds (its detach, after the
    /// runtime relayed the releases).
    pub(super) fn release_satellite_leases_for(&mut self, client: ClientId) {
        self.leases.retain(|_, lease| lease.holder != client);
    }

    /// Mark an `ACQUIRE_INPUT` relay in flight. Clear it on failure; on
    /// success the next mirrored event clears it.
    pub(crate) fn mark_satellite_lease_acquire_pending(
        &mut self,
        host: SatelliteHost,
        terminal: u32,
    ) {
        self.acquire_pending.insert((host, terminal));
    }

    /// Clear the pending mark set by
    /// [`Self::mark_satellite_lease_acquire_pending`] after a failed relay.
    pub(crate) fn clear_satellite_lease_acquire_pending(
        &mut self,
        host: &SatelliteHost,
        terminal: u32,
    ) {
        self.acquire_pending.remove(&(host.clone(), terminal));
    }

    /// Forget the mirror-ordering bookkeeping for a satellite terminal that
    /// closed; no event for it can arrive again. Without this the maps keep
    /// one entry per satellite terminal that ever held a lease, for the
    /// hub's lifetime, and a satellite naming fresh ids grows them at will.
    pub(crate) fn forget_satellite_lease_terminal(&mut self, host: &SatelliteHost, terminal: u32) {
        let key = (host.clone(), terminal);
        self.last_mirrored_seq.remove(&key);
        self.acquire_pending.remove(&key);
    }

    /// Entries in the satellite lease-mirror ordering state.
    #[cfg(test)]
    pub(crate) fn satellite_lease_mirror_entries(&self) -> usize {
        self.last_mirrored_seq.len() + self.acquire_pending.len()
    }

    /// Mirror a satellite `terminal_control` into the hub ledger (the
    /// satellite owns the timer). `is_end` is true for Released/Expired.
    /// Returns the evicted holder when an end was applied.
    ///
    /// The acquire reply can beat an earlier Released/Expired over the link:
    ///
    /// * **Pending**: the first event after a relayed acquire consumes the
    ///   mark; an end arriving then is stale and swallowed.
    /// * **Seq**: otherwise an end applies only if its `seq` is newer than
    ///   the last mirrored Acquired/Seized.
    ///
    /// A lost confirming event could let one genuine end be swallowed, but
    /// only once.
    pub(crate) fn mirror_satellite_lease_event(
        &mut self,
        host: &SatelliteHost,
        terminal: u32,
        is_end: bool,
        seq: Option<u64>,
    ) -> Option<ClientId> {
        let key = (host.clone(), terminal);
        let was_pending = self.acquire_pending.remove(&key);
        if was_pending && is_end {
            return None;
        }
        if !was_pending
            && is_end
            && let Some(seq) = seq
            && let Some(last) = self.last_mirrored_seq.get(&key)
            && seq <= *last
        {
            return None;
        }
        if !is_end {
            if let Some(seq) = seq {
                self.last_mirrored_seq.insert(key, seq);
            }
            return None;
        }
        self.leases.remove(&key).map(|lease| lease.holder)
    }
}
