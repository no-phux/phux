//! The federation-hub handles a server holds while it is acting as a hub
//! (phux-v45, ADR-0007): the validated satellite table, the per-satellite
//! link statuses, and the per-satellite frame relays.
//!
//! Three fields that were flat on [`super::ServerState`] live here because
//! they share one lifetime *and* one predicate. All three are `None` on
//! every non-hub server and all three are installed once, together, during
//! `ServerRuntime::run_async` — the table right after
//! [`crate::hub::resolve_hub_table`] succeeds, the statuses and relays
//! alongside the link supervisors that publish into them. `None` is
//! therefore not "not set yet" but "this server is not a hub", which is the
//! gate every read here is really asking about; keeping the three together
//! makes that one answer rather than three independently-drifting ones.
//!
//! # Ownership boundary
//!
//! This type owns the *handles*, not the federation protocol around them.
//! Route resolution, the relay round trips, and the `LIST` aggregation live
//! in `runtime::commands` / `runtime::client` / `crate::hub` and reach in
//! through the delegating accessors on `ServerState` (see `state::hub`).
//! Nothing here interprets a [`crate::hub::HubTable`] beyond handing it
//! back.
//!
//! All three fields are private: like `state::lease_table`, nothing on
//! `ServerState` needs to borrow-split a hub handle against another field,
//! so every read and write goes through a method here.
//!
//! Nothing here is `async` and nothing awaits, so the state lock can never
//! be held across a suspension point through this type.
//!
//! The struct and every method are `pub(super)`: the accessors the runtime
//! calls stay on `ServerState`, so the crate's public surface is unchanged
//! and all three handles stay exactly as unreachable from outside `state`
//! as they were as private fields.
//!
//! Not to be confused with [`crate::hub`], the module holding the hub
//! machinery these handles point at.

/// Every federation-hub handle the server owns, all `None` off-hub.
///
/// Held as a single field on [`super::ServerState`]. Not thread-safe on
/// its own; the surrounding `Mutex<ServerState>` provides synchronization.
#[derive(Debug)]
pub(super) struct HubState {
    /// Validated satellite table for a federation hub (phux-v45.1,
    /// ADR-0007). `None` on every non-hub server — the table is never read
    /// outside hub mode. Set once at startup by the runtime via
    /// [`Self::set_table`] after [`crate::hub::resolve_hub_table`]
    /// succeeds.
    table: Option<crate::hub::HubTable>,
    /// Per-satellite link statuses published by the hub's outbound link
    /// supervisors (phux-v45.3). `None` on every non-hub server. Set once
    /// at startup via [`Self::set_link_statuses`] alongside the link spawn;
    /// the handle is the read surface a future `LIST` aggregation
    /// (phux-v45.5) consumes.
    link_statuses: Option<crate::hub::link::HubLinkStatuses>,
    /// Per-satellite frame-relay handles (phux-v45.4, ADR-0007 §4).
    /// `None` on every non-hub server. Set once at hub startup via
    /// [`Self::set_relays`] alongside the link spawn; command and input
    /// dispatch resolve `TerminalId::Satellite { host, .. }` through it to
    /// the owning link's relay mailbox.
    relays: Option<crate::hub::relay::HubRelays>,
}

impl Default for HubState {
    fn default() -> Self {
        Self::new()
    }
}

impl HubState {
    /// Build the off-hub state: no table, no statuses, no relays.
    #[must_use]
    pub(super) const fn new() -> Self {
        Self {
            table: None,
            link_statuses: None,
            relays: None,
        }
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

    /// Install the shared per-satellite link-status handle (phux-v45.3).
    pub(super) fn set_link_statuses(&mut self, statuses: crate::hub::link::HubLinkStatuses) {
        self.link_statuses = Some(statuses);
    }

    /// Read the per-satellite link statuses set by
    /// [`Self::set_link_statuses`]. `None` on a non-hub server.
    #[must_use]
    pub(super) const fn link_statuses(&self) -> Option<&crate::hub::link::HubLinkStatuses> {
        self.link_statuses.as_ref()
    }

    /// Install the shared per-satellite frame-relay registry (phux-v45.4).
    pub(super) fn set_relays(&mut self, relays: crate::hub::relay::HubRelays) {
        self.relays = Some(relays);
    }

    /// The relay handle for satellite `host`, or `None` when this server is
    /// not a hub or `host` is not in its table — the caller's
    /// `UnsupportedSatelliteRoute` signal.
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
