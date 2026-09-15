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
    /// HELLO (`docs/spec/workload-auth.md` §7), and wake the revocation
    /// watcher when the connection is one it must watch.
    ///
    /// A connection revoked while its HELLO was being authorized keeps its
    /// revoked placeholder: the grant never becomes live.
    pub fn set_connection_grant(
        &mut self,
        client_id: ClientId,
        grant: crate::policy::ConnectionGrant,
    ) {
        if self.connection_revoked(client_id) {
            return;
        }
        let watched = !grant.is_owner() || self.bearer_admission(client_id).is_some();
        self.clients.grants.insert(client_id, grant);
        if watched {
            self.clients.revocation_wake.notify_one();
        }
    }

    /// The pairing-store admission retained for this connection, if a
    /// bearer token admitted it.
    #[must_use]
    pub(crate) fn bearer_admission(
        &self,
        client_id: ClientId,
    ) -> Option<&crate::auth::BearerAdmission> {
        self.clients
            .peer_identities
            .get(&client_id)
            .and_then(|identity| identity.bearer.as_ref())
    }

    /// Whether this connection's authority was withdrawn while it was live.
    #[must_use]
    pub fn connection_revoked(&self, client_id: ClientId) -> bool {
        self.connection_grant(client_id)
            .is_some_and(|grant| grant.revocation().is_some())
    }

    /// Register the sending half of this connection's revocation signal.
    pub(crate) fn set_revocation_signal(
        &mut self,
        client_id: ClientId,
        signal: tokio::sync::watch::Sender<Option<crate::policy::Goodbye>>,
    ) {
        self.clients.revocation_signals.insert(client_id, signal);
    }

    /// The handle that wakes the revocation watcher.
    #[must_use]
    pub(crate) fn revocation_wake(&self) -> std::sync::Arc<tokio::sync::Notify> {
        std::sync::Arc::clone(&self.clients.revocation_wake)
    }

    /// `workload-auth.md` §7 step 1: withdraw the connection's authority, so
    /// no guard admits anything from it again, and tell its writer which
    /// goodbye it owes. A connection with no grant yet is still in its
    /// handshake: it gets a revoked placeholder and a silent close.
    pub(crate) fn mark_connection_revoked(
        &mut self,
        client_id: ClientId,
        revocation: crate::policy::Revocation,
    ) {
        use crate::policy::{ConnectionGrant, Goodbye};
        let goodbye = if let Some(grant) = self.clients.grants.get_mut(&client_id) {
            grant.revoke(revocation);
            Goodbye::Announce(revocation)
        } else {
            self.clients
                .grants
                .insert(client_id, ConnectionGrant::revoked_placeholder(revocation));
            Goodbye::Silent
        };
        if let Some(signal) = self.clients.revocation_signals.get(&client_id) {
            signal.send_replace(Some(goodbye));
        }
    }

    /// Every live connection the revocation watcher must re-judge: each
    /// scoped grant, and each grant a pairing-store bearer admitted.
    #[must_use]
    pub(crate) fn watched_connections(&self) -> Vec<crate::policy::WatchedConnection> {
        self.clients
            .grants
            .iter()
            .filter(|(_, grant)| grant.revocation().is_none())
            .filter_map(|(client, grant)| {
                let bearer = self.bearer_admission(*client).cloned();
                (!grant.is_owner() || bearer.is_some()).then(|| crate::policy::WatchedConnection {
                    client: *client,
                    grant: grant.clone(),
                    bearer,
                })
            })
            .collect()
    }

    /// Record that a live scoped grant still holds under a newer registry
    /// state.
    pub(crate) fn refresh_connection_grant(
        &mut self,
        client_id: ClientId,
        stamp: crate::policy::RegistryStamp,
    ) {
        if let Some(grant) = self.clients.grants.get_mut(&client_id) {
            grant.refresh(stamp);
        }
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
