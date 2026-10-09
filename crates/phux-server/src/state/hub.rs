use super::ServerState;

impl ServerState {
    /// Install the validated satellite table (hub mode, at startup).
    pub fn set_hub_table(&mut self, table: crate::hub::HubTable) {
        self.hub.set_table(table);
    }

    /// Read the hub satellite table set by [`Self::set_hub_table`].
    /// `None` on a non-hub server.
    #[must_use]
    pub const fn hub_table(&self) -> Option<&crate::hub::HubTable> {
        self.hub.table()
    }

    /// Install the relay registry (hub startup).
    pub(crate) fn set_hub_relays(&mut self, relays: crate::hub::relay::HubRelays) {
        self.hub.set_relays(relays);
    }

    /// Install the running link supervisors and the registry source a live
    /// reload re-reads (hub startup).
    pub(crate) fn set_hub_links(
        &mut self,
        links: crate::hub::link::HubLinks,
        source: Option<crate::hub::SatelliteSource>,
    ) {
        self.hub.set_links(links, source);
    }

    /// The registry source [`crate::hub::reload_satellites`] re-reads;
    /// `None` when the embedder supplied none.
    #[must_use]
    pub(crate) fn hub_satellite_source(&self) -> Option<crate::hub::SatelliteSource> {
        self.hub.source()
    }

    /// Arm reload before any link exists, so a later doorbell can promote
    /// this server into a hub without restarting panes.
    pub(crate) fn arm_hub_reload(
        &mut self,
        source: Option<crate::hub::SatelliteSource>,
        cancel: tokio_util::sync::CancellationToken,
    ) {
        self.hub.arm_reload(source, cancel);
    }

    /// Whether satellite link supervisors are installed.
    #[must_use]
    pub(crate) const fn hub_has_links(&self) -> bool {
        self.hub.has_links()
    }

    /// Install empty link supervisors when a doorbell is about to add the
    /// first satellites. `false` when reload was not armed with a token.
    pub(crate) fn hub_ensure_links(&mut self, ssh_program: std::ffi::OsString) -> bool {
        self.hub.ensure_links(ssh_program)
    }

    /// Swap in a reloaded satellite table, starting, stopping, or redialing
    /// only the links that differ. `journal` is this state's own handle,
    /// cloned into each new link.
    pub(crate) fn hub_replace_table(
        &mut self,
        next: crate::hub::HubTable,
        journal: &super::SharedState,
    ) -> crate::hub::TableDiff {
        self.hub.replace_table(next, journal)
    }

    /// The relay for `host`; `None` off-hub or for an unknown host.
    #[must_use]
    pub(crate) fn hub_relay(
        &self,
        host: &phux_protocol::ids::SatelliteHost,
    ) -> Option<crate::hub::relay::RelayHandle> {
        self.hub.relay(host)
    }

    /// Record what `host` advertised on its current link (ADR-0127), from
    /// the link's relay session when it negotiates.
    pub(crate) fn set_satellite_features(
        &mut self,
        host: phux_protocol::ids::SatelliteHost,
        features: phux_protocol::caps::ServerFeatureSet,
    ) {
        self.hub.set_satellite_features(host, features);
    }

    /// Whether `host` advertised `feature` on its current link (ADR-0127).
    #[must_use]
    pub(crate) fn satellite_advertises(
        &self,
        host: &phux_protocol::ids::SatelliteHost,
        feature: phux_protocol::caps::ServerFeature,
    ) -> bool {
        self.hub.satellite_features(host).contains(feature)
    }

    /// Every satellite relay handle (detach fan-out); empty off-hub.
    #[must_use]
    pub(crate) fn hub_relays_all(&self) -> Vec<crate::hub::relay::RelayHandle> {
        self.hub.relays_all()
    }

    /// Remember that hub consumer `spawner` asked for `host`'s resource `id`,
    /// bound to `instance` when the spawn asked for binding (ADR-0109).
    pub(crate) fn hub_record_satellite_spawn(
        &mut self,
        host: phux_protocol::ids::SatelliteHost,
        id: u32,
        instance: Option<phux_protocol::ids::ServerInstance>,
        spawner: super::ClientId,
    ) {
        self.satellite_spawns
            .record_spawn(host, id, instance, spawner);
    }

    /// Note that hub consumer `client` is attaching or using `host`'s
    /// resource `id` (ADR-0109).
    pub(crate) fn hub_note_satellite_use(
        &mut self,
        host: &phux_protocol::ids::SatelliteHost,
        id: u32,
        client: super::ClientId,
    ) {
        self.satellite_spawns.note_use(host, id, client);
    }

    /// Whether this hub can vouch that `host`'s `id` is unattached since
    /// spawn under `instance` (ADR-0109).
    #[must_use]
    pub(crate) fn hub_vouches_unattached(
        &self,
        host: &phux_protocol::ids::SatelliteHost,
        id: u32,
        instance: phux_protocol::ids::ServerInstance,
    ) -> bool {
        self.satellite_spawns.vouches_unattached(host, id, instance)
    }
}
