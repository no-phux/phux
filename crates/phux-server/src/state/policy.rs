use super::{ClientId, ServerState};

impl ServerState {
    /// Set the policy extension bundle. Called once at server startup.
    pub fn set_policy_bundle(&mut self, bundle: crate::policy::PolicyBundle) {
        self.config.policy_bundle = bundle;
    }

    /// Read the policy extension bundle.
    #[must_use]
    pub fn policy_bundle(&self) -> &crate::policy::PolicyBundle {
        &self.config.policy_bundle
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
