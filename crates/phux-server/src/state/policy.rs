use super::{ClientId, ServerState};

impl ServerState {
    /// `client` as the input log names it: its credential id, `owner` on
    /// the owner's socket, else `anonymous`.
    #[must_use]
    pub fn input_sender(&self, client: ClientId) -> String {
        if let Some(credential) = self.clients().authenticated_credential(client) {
            return credential.id.clone();
        }
        let owner = self
            .clients()
            .peer_identity(client)
            .is_some_and(|peer| peer.transport == phux_protocol::policy::TransportType::UnixSocket);
        if owner { "owner" } else { "anonymous" }.to_owned()
    }

    /// What a hub stamps on document input it relays for `client`
    /// (`docs/authority.md#relayed-identity`): [`Self::input_sender`] and
    /// its `HELLO` name, each cut to `MAX_ON_BEHALF_OF` bytes.
    #[must_use]
    pub fn relayed_for(&self, client: ClientId) -> phux_protocol::wire::frame::OnBehalfOf {
        let cut = |text: &str| {
            let mut end = text.len().min(phux_protocol::wire::frame::MAX_ON_BEHALF_OF);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text[..end].to_owned()
        };
        phux_protocol::wire::frame::OnBehalfOf {
            credential: cut(&self.input_sender(client)),
            name: cut(self.client_name(client).unwrap_or_default()),
        }
    }

    /// The audit actor for `client_id`; the server itself when unknown.
    #[must_use]
    pub fn audit_actor(&self, client_id: ClientId) -> crate::audit::AuditActor {
        let mut actor = self
            .clients
            .connection_identity(client_id)
            .map_or_else(crate::audit::AuditActor::server, crate::audit::audit_actor);
        actor.on_behalf_of = self
            .connection_grant(client_id)
            .and_then(crate::policy::ConnectionGrant::on_behalf_of)
            .map(|principal| crate::policy::principal_name(principal).to_owned());
        actor
    }

    /// Audit `client_id`'s connection opening or closing.
    pub fn audit_connection(&self, client_id: ClientId, kind: crate::audit::Kind) {
        if !crate::audit::enabled() {
            return;
        }
        let peer = self.clients().peer_identity(client_id);
        crate::audit::emit(
            kind,
            self.audit_actor(client_id),
            if kind == crate::audit::Kind::ConnectionOpen {
                "open"
            } else {
                "close"
            },
            None,
            crate::audit::Outcome::Allowed,
            serde_json::json!({
                "client": client_id.0,
                "uid": peer.map(|peer| peer.uid),
                "pid": peer.and_then(|peer| peer.pid),
                "source": peer.and_then(|peer| peer.source_addr).map(|addr| addr.to_string()),
            }),
        );
    }

    /// Whether `client_id` may feed a producer-fed stream (ADR-0103 §3):
    /// local owner-socket clients only; unstamped connections are refused.
    #[must_use]
    pub fn client_may_produce(&self, client_id: ClientId) -> bool {
        matches!(
            self.clients()
                .peer_identity(client_id)
                .map(|peer| peer.transport),
            Some(
                phux_protocol::policy::TransportType::UnixSocket
                    | phux_protocol::policy::TransportType::Localhost
            )
        )
    }

    /// Remove a peer identity and its grant on disconnect (not detach).
    pub fn remove_peer_identity(&mut self, client_id: ClientId) {
        self.clients.remove_peer_identity(client_id);
        if self
            .native_agents()
            .relaunch_proposals
            .contains_key(&client_id)
        {
            self.native_agents_mut()
                .relaunch_proposals
                .remove(&client_id);
        }
        self.clients.grants.remove(&client_id);
    }

    /// Retain the connection's HELLO grant and wake the revocation watcher.
    /// A connection revoked mid-HELLO keeps its revoked placeholder.
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

    /// Withdraw a connection's authority and set its goodbye (revocation step 1); a
    /// connection still in its handshake gets a silent close.
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

    /// Whether an uncorrelated `PERMISSION_DENIED` may be sent now (rate
    /// limited).
    pub fn admit_denial_error(&mut self, client_id: ClientId) -> bool {
        let now = std::time::Instant::now();
        self.clients
            .grants
            .get_mut(&client_id)
            .is_some_and(|grant| grant.admit_denial_error(now))
    }
}
