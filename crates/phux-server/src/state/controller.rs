//! The `phux.controller/v1` record: which connection last typed into a
//! Terminal (`docs/building.md#who-is-typing`).

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{
    AUTH_ROUTE_LOOPBACK_QUIC, AUTH_ROUTE_LOOPBACK_WEBTRANSPORT, AUTH_ROUTE_LOOPBACK_WS,
    AUTH_ROUTE_UDS, RESOURCE_CONTROLLER_KEY, Scope,
};

use super::{ClientId, ServerState};

impl ServerState {
    /// `client` typed (a key or a paste) into the local `terminal`. Publishes
    /// the record only when the typing connection changed, so the per-key
    /// cost is one map lookup.
    // ponytail: on a satellite the typing connection is the hub's link, so
    // the record names the hub, not the person's device. Carrying the origin
    // through the hub needs a field on the hub link.
    pub fn note_controller(&mut self, terminal: &WireResourceId, client: ClientId) {
        if self.clients.controllers.get(terminal) == Some(&client) {
            return;
        }
        let Some(route) = crate::runtime::route_for(self, client) else {
            return;
        };
        let local = [
            AUTH_ROUTE_UDS,
            AUTH_ROUTE_LOOPBACK_QUIC,
            AUTH_ROUTE_LOOPBACK_WS,
            AUTH_ROUTE_LOOPBACK_WEBTRANSPORT,
        ]
        .contains(&route);
        let record = serde_json::json!({
            "client": crate::runtime::wire_client(client).get(),
            "client_name": self.client_name(client),
            "route": route,
            "remote": !local,
        });
        self.clients.controllers.insert(terminal.clone(), client);
        self.metadata_set(
            &Scope::Resource(terminal.clone()),
            RESOURCE_CONTROLLER_KEY,
            record.to_string().into_bytes(),
        );
    }

    /// Withdraw every record naming `client`, whose connection closed.
    pub(super) fn clear_controller(&mut self, client: ClientId) {
        let mut gone = Vec::new();
        self.clients.controllers.retain(|terminal, holder| {
            let keep = *holder != client;
            if !keep {
                gone.push(terminal.clone());
            }
            keep
        });
        for terminal in gone {
            self.metadata_delete(&Scope::Resource(terminal), RESOURCE_CONTROLLER_KEY);
        }
    }
}
