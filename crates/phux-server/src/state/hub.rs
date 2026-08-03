use super::ServerState;

impl ServerState {
    /// Install the validated hub satellite table (phux-v45.1). Called once
    /// at server startup, only in hub mode, after
    /// [`crate::hub::resolve_hub_table`] succeeds.
    pub fn set_hub_table(&mut self, table: crate::hub::HubTable) {
        self.hub.set_table(table);
    }

    /// Read the hub satellite table set by [`Self::set_hub_table`].
    /// `None` on a non-hub server.
    #[must_use]
    pub const fn hub_table(&self) -> Option<&crate::hub::HubTable> {
        self.hub.table()
    }

    /// Install the shared per-satellite link-status handle (phux-v45.3).
    /// Called once at hub startup, alongside spawning the link
    /// supervisors that publish into it.
    pub fn set_hub_link_statuses(&mut self, statuses: crate::hub::link::HubLinkStatuses) {
        self.hub.set_link_statuses(statuses);
    }

    /// Read the per-satellite link statuses set by
    /// [`Self::set_hub_link_statuses`]. `None` on a non-hub server.
    #[must_use]
    pub const fn hub_link_statuses(&self) -> Option<&crate::hub::link::HubLinkStatuses> {
        self.hub.link_statuses()
    }

    /// Install the shared per-satellite frame-relay registry
    /// (phux-v45.4). Called once at hub startup, alongside spawning the
    /// link supervisors that drain its mailboxes.
    pub(crate) fn set_hub_relays(&mut self, relays: crate::hub::relay::HubRelays) {
        self.hub.set_relays(relays);
    }

    /// The relay handle for satellite `host`, or `None` when this server
    /// is not a hub or `host` is not in its table — the caller's
    /// `UnsupportedSatelliteRoute` signal.
    #[must_use]
    pub(crate) fn hub_relay(
        &self,
        host: &phux_protocol::ids::SatelliteHost,
    ) -> Option<crate::hub::relay::RelayHandle> {
        self.hub.relay(host)
    }

    /// Every satellite relay handle (detach fan-out); empty off-hub.
    #[must_use]
    pub(crate) fn hub_relays_all(&self) -> Vec<crate::hub::relay::RelayHandle> {
        self.hub.relays_all()
    }
}
