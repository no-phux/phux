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

    /// Store transport identity and structured credential attestation.
    pub fn set_connection_identity(
        &mut self,
        client_id: ClientId,
        identity: crate::auth::ConnectionIdentity,
    ) {
        self.clients.set_connection_identity(client_id, identity);
    }

    /// Look up a peer identity by client id.
    #[must_use]
    pub fn peer_identity(
        &self,
        client_id: ClientId,
    ) -> Option<&phux_protocol::policy::PeerIdentity> {
        self.clients.peer_identity(client_id)
    }

    /// Look up the structured credential captured at connection establishment.
    #[must_use]
    pub fn authenticated_credential(
        &self,
        client_id: ClientId,
    ) -> Option<&crate::auth::AuthenticatedCredential> {
        self.clients.authenticated_credential(client_id)
    }

    /// Record the ssh origin a same-uid `phux stdio-bridge` announced in
    /// HELLO. The caller has already checked the peer may make the claim
    /// (`runtime::whoami`); the value only relabels the whoami route.
    pub fn set_ssh_origin(
        &mut self,
        client_id: ClientId,
        origin: phux_protocol::wire::ssh_origin::SshOrigin,
    ) {
        self.clients.set_ssh_origin(client_id, origin);
    }

    /// The ssh origin recorded for this connection, if any.
    #[must_use]
    pub fn ssh_origin(
        &self,
        client_id: ClientId,
    ) -> Option<phux_protocol::wire::ssh_origin::SshOrigin> {
        self.clients.ssh_origin(client_id)
    }

    /// Whether `client_id` may feed a producer-fed resource's stream
    /// (ADR-0103 §3).
    ///
    /// The producer holds the ADR-0098 `Input` verb on the resource, and
    /// under the current policy that is every owner-socket client and no
    /// remote one: a client that reached this server over a network
    /// transport can drive a Terminal it has been granted, but writing an
    /// agent session's stream is asserting what a *local* harness did, and
    /// a remote peer cannot be the author of that.
    ///
    /// A connection whose peer identity was never stamped is refused: the
    /// answer is "not established", and for an authorship claim that is a
    /// no.
    #[must_use]
    pub fn client_may_produce(&self, client_id: ClientId) -> bool {
        matches!(
            self.peer_identity(client_id).map(|peer| peer.transport),
            Some(
                phux_protocol::policy::TransportType::UnixSocket
                    | phux_protocol::policy::TransportType::Localhost
            )
        )
    }

    /// Remove a peer identity, and the grant minted from it, when a client
    /// *disconnects* — not when it detaches. Peer identity is stamped by the
    /// accepting transport and cannot be re-established on a live
    /// connection, so [`Self::forget_connection`] is its only caller.
    pub fn remove_peer_identity(&mut self, client_id: ClientId) {
        self.clients.remove_peer_identity(client_id);
        self.clients.grants.remove(&client_id);
    }

    /// Retain the grant the policy engine minted for this connection at
    /// HELLO (`docs/spec/workload-auth.md` §7).
    pub fn set_connection_grant(
        &mut self,
        client_id: ClientId,
        grant: crate::policy::ConnectionGrant,
    ) {
        self.clients.grants.insert(client_id, grant);
    }

    /// The grant this connection holds; `None` before HELLO, and for a
    /// connection the dispatch guard therefore refuses everything.
    #[must_use]
    pub fn connection_grant(&self, client_id: ClientId) -> Option<&crate::policy::ConnectionGrant> {
        self.clients.grants.get(&client_id)
    }

    /// Whether an uncorrelated `PERMISSION_DENIED` may be sent to this
    /// connection now: at most one per
    /// [`crate::policy::DENIAL_ERROR_INTERVAL`]. A connection with no grant
    /// gets none.
    pub fn admit_denial_error(&mut self, client_id: ClientId) -> bool {
        let now = std::time::Instant::now();
        self.clients
            .grants
            .get_mut(&client_id)
            .is_some_and(|grant| grant.admit_denial_error(now))
    }

    /// The wire id already interned for a local resource, without
    /// allocating one: the dispatch guard reads, it never mints ids.
    #[must_use]
    pub(crate) fn terminal_wire_of(
        &self,
        terminal: phux_core::ids::ResourceId,
    ) -> Option<phux_protocol::ids::ResourceId> {
        self.idspace.terminal_wire(terminal).cloned()
    }

    /// Record the authorization posture the server started in. Set once at
    /// startup.
    pub fn set_policy_posture(&mut self, posture: crate::policy::PolicyPosture) {
        self.config.policy_posture = posture;
    }

    /// The authorization posture the server started in.
    #[must_use]
    pub const fn policy_posture(&self) -> crate::policy::PolicyPosture {
        self.config.policy_posture
    }

    /// Whether TLS listeners must require a workload client certificate.
    #[must_use]
    pub const fn workload_mtls_required(&self) -> bool {
        self.config.policy_posture.requires_workload_mtls()
    }
}
