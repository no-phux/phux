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
