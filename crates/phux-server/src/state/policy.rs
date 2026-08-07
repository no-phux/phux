use super::{ClientId, ServerState};

impl ServerState {
    /// Install the HELLO authorization engine. Called once at server
    /// startup, before the accept loops start.
    pub fn set_policy_engine(&mut self, engine: std::sync::Arc<dyn crate::policy::PolicyEngine>) {
        self.config.policy_engine = engine;
    }

    /// Read the HELLO authorization engine. Defaults to
    /// [`crate::policy::PermissivePolicy`] (ADR-0072).
    #[must_use]
    pub fn policy_engine(&self) -> &std::sync::Arc<dyn crate::policy::PolicyEngine> {
        &self.config.policy_engine
    }

    /// Store a peer identity for a client.
    pub fn set_peer_identity(
        &mut self,
        client_id: ClientId,
        identity: phux_protocol::policy::PeerIdentity,
    ) {
        self.clients.set_peer_identity(client_id, identity);
    }

    /// Look up a peer identity by client id.
    #[must_use]
    pub fn peer_identity(
        &self,
        client_id: ClientId,
    ) -> Option<&phux_protocol::policy::PeerIdentity> {
        self.clients.peer_identity(client_id)
    }

    /// Remove a peer identity when a client disconnects.
    pub fn remove_peer_identity(&mut self, client_id: ClientId) {
        self.clients.remove_peer_identity(client_id);
    }
}
