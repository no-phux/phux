//! Hub handles (ADR-0007): the satellite table, relays, and link supervisors.
//! All are installed together at startup and are `None` exactly when
//! the server is not a hub; a registry reload swaps the table in place. The
//! federation protocol lives in the runtime and [`crate::hub`]. Everything is
//! `pub(super)` and sync.

/// Every hub handle, all `None` off-hub.
#[derive(Debug)]
pub(super) struct HubState {
    /// Validated satellite table.
    table: Option<crate::hub::HubTable>,
    /// Per-satellite relay handles, used to route `ResourceId::Satellite`.
    relays: Option<crate::hub::relay::HubRelays>,
    /// The running link supervisors, so a reload can start and stop links.
    links: Option<crate::hub::link::HubLinks>,
    /// Where a reload re-reads `[[satellites]]`; `None` disables reload.
    source: Option<crate::hub::SatelliteSource>,
    /// What each satellite advertised on its current link (ADR-0127), set by
    /// the link's relay session when it negotiates. Empty off-hub.
    satellite_features: Vec<(
        phux_protocol::ids::SatelliteHost,
        phux_protocol::caps::ServerFeatureSet,
    )>,
}

impl Default for HubState {
    fn default() -> Self {
        Self::new()
    }
}

impl HubState {
    /// Build the off-hub state: no table, no relays.
    #[must_use]
    pub(super) const fn new() -> Self {
        Self {
            table: None,
            relays: None,
            links: None,
            source: None,
            satellite_features: Vec::new(),
        }
    }

    /// Install the running link supervisors and the registry source a reload
    /// re-reads (hub startup).
    pub(super) fn set_links(
        &mut self,
        links: crate::hub::link::HubLinks,
        source: Option<crate::hub::SatelliteSource>,
    ) {
        self.links = Some(links);
        self.source = source;
    }

    /// The registry source a reload re-reads; `None` off-hub.
    pub(super) fn source(&self) -> Option<crate::hub::SatelliteSource> {
        self.links.as_ref().and(self.source.clone())
    }

    /// Swap in `next` and start, stop, or redial exactly the links that
    /// differ from the running table. Off-hub nothing changes.
    pub(super) fn replace_table(
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

    /// Record what `host` advertised on its newest link, replacing any
    /// earlier link's.
    pub(super) fn set_satellite_features(
        &mut self,
        host: phux_protocol::ids::SatelliteHost,
        features: phux_protocol::caps::ServerFeatureSet,
    ) {
        self.satellite_features.retain(|(known, _)| *known != host);
        self.satellite_features.push((host, features));
    }

    /// What `host` advertised; empty for a host with no negotiated link.
    pub(super) fn satellite_features(
        &self,
        host: &phux_protocol::ids::SatelliteHost,
    ) -> phux_protocol::caps::ServerFeatureSet {
        self.satellite_features
            .iter()
            .find(|(known, _)| known == host)
            .map(|(_, features)| *features)
            .unwrap_or_default()
    }

    /// Install the validated hub satellite table (phux-v45.1).
    pub(super) fn set_table(&mut self, table: crate::hub::HubTable) {
        self.table = Some(table);
    }

    /// Read the hub satellite table set by [`Self::set_table`]. `None` on a
    /// non-hub server.
    #[must_use]
    pub(super) const fn table(&self) -> Option<&crate::hub::HubTable> {
        self.table.as_ref()
    }

    /// Install the shared per-satellite frame-relay registry (phux-v45.4).
    pub(super) fn set_relays(&mut self, relays: crate::hub::relay::HubRelays) {
        self.relays = Some(relays);
    }

    /// The relay for `host`; `None` off-hub or for an unknown host.
    #[must_use]
    pub(super) fn relay(
        &self,
        host: &phux_protocol::ids::SatelliteHost,
    ) -> Option<crate::hub::relay::RelayHandle> {
        self.relays.as_ref().and_then(|relays| relays.get(host))
    }

    /// Every satellite relay handle (detach fan-out); empty off-hub.
    #[must_use]
    pub(super) fn relays_all(&self) -> Vec<crate::hub::relay::RelayHandle> {
        self.relays
            .as_ref()
            .map(crate::hub::relay::HubRelays::all)
            .unwrap_or_default()
    }
}
