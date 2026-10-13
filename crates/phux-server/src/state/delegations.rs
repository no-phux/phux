//! Minting, admitting, and withdrawing delegated agent credentials
//! (ADR-0163 §5); the ledger and its rules are [`crate::delegation`].

use chrono::{DateTime, Utc};
use phux_core::PrincipalId;
use phux_core::ids::SessionId;
use phux_protocol::kinds::{Verb, Verbs};

use super::{ClientId, ServerState};
use crate::delegation::{
    AGENT_VERBS, CarriedDelegation, DelegatedCredential, DelegationError, DelegationRequest,
    MAX_LIFETIME,
};
use crate::policy::ConnectionGrant;
use crate::upgrade::blob::DelegationBlob;

/// Why a carried delegated credential was not restored after a hot upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UnrestoredDelegation {
    /// Its id is not a delegated credential id, or it carries a verb no
    /// agent may hold.
    #[error("the carried credential is malformed")]
    Malformed,
    /// It lapsed during the upgrade.
    #[error("the credential expired")]
    Expired,
    /// Its session did not survive the upgrade.
    #[error("its session is gone")]
    NoSession,
    /// The human it acts for, or the principal that minted it, no longer
    /// holds a credential.
    #[error("its launcher no longer holds a grant")]
    NoLauncher,
}

impl ServerState {
    /// Mint a credential `launcher` delegates to an agent on `session`.
    ///
    /// Only narrows: every requested verb must be an agent verb
    /// ([`crate::delegation::AGENT_VERBS`]) that the launcher's own grant
    /// admits on the whole session, and the lifetime ends no later than the
    /// launcher's. `on_behalf_of` is the launcher's human: a delegate
    /// re-delegating passes on the human it acts for.
    ///
    /// # Errors
    ///
    /// The [`DelegationError`]; nothing was minted.
    pub fn mint_delegation(
        &mut self,
        launcher: ClientId,
        session: SessionId,
        request: DelegationRequest,
        now: DateTime<Utc>,
    ) -> Result<DelegatedCredential, DelegationError> {
        let grant = self
            .connection_grant(launcher)
            .filter(|grant| grant.revocation().is_none() && !grant.is_expired_at(now))
            .cloned()
            .ok_or(DelegationError::NoLauncherGrant)?;
        if self.registry().session(session).is_none() {
            return Err(DelegationError::NoSession);
        }
        if request.verbs.is_empty() {
            return Err(DelegationError::NoVerbs);
        }
        if let Some(verb) = Verb::ALL.into_iter().find(|verb| {
            request.verbs.contains(*verb) && !crate::delegation::AGENT_VERBS.contains(*verb)
        }) {
            return Err(DelegationError::Widens(verb));
        }
        let group = self.idspace.intern_session(session).get();
        if let Some(verb) = Verb::ALL
            .into_iter()
            .find(|verb| request.verbs.contains(*verb) && !grant.admits_session(*verb, group))
        {
            return Err(DelegationError::LauncherLacks(verb));
        }
        let lifetime = chrono::Duration::from_std(request.lifetime.min(MAX_LIFETIME))
            .map_err(|_| DelegationError::Lifetime)?;
        let expires_at = [Some(now + lifetime), grant.expires_at]
            .into_iter()
            .flatten()
            .min()
            .ok_or(DelegationError::Lifetime)?;
        if expires_at <= now {
            return Err(DelegationError::Lifetime);
        }
        let credential = DelegatedCredential {
            id: crate::delegation::mint_id()?,
            session,
            group,
            verbs: request.verbs,
            on_behalf_of: grant.answering_principal().clone(),
            delegator: grant.principal().clone(),
            expires_at,
        };
        self.delegations.insert(credential.clone());
        Ok(credential)
    }

    /// The grant a connection presenting delegated credential `id` gets at
    /// `now`; `None` once it is withdrawn or lapsed.
    #[must_use]
    pub fn delegated_grant(&self, id: &str, now: DateTime<Utc>) -> Option<ConnectionGrant> {
        let credential = self.delegations.live(id, now)?;
        ConnectionGrant::delegated(
            credential.scopes(),
            credential.principal(),
            credential.on_behalf_of.clone(),
            credential.expires_at,
        )
        .ok()
    }

    /// The live delegated credential `id`, if any.
    #[must_use]
    pub fn delegation(&self, id: &str, now: DateTime<Utc>) -> Option<&DelegatedCredential> {
        self.delegations.live(id, now)
    }

    /// Withdraw delegated credential `id`. Returns every live connection
    /// acting as it, for the caller to revoke (`workload-auth.md` §7); empty
    /// when `id` named nothing.
    pub fn withdraw_delegation(&mut self, id: &str) -> Vec<ClientId> {
        let Some(credential) = self.delegations.remove(id) else {
            return Vec::new();
        };
        let principal = credential.principal();
        self.clients
            .grants
            .iter()
            .filter(|(_, grant)| grant.principal() == &principal)
            .map(|(client, _)| *client)
            .collect()
    }

    /// Revoke every delegated credential scoped to `session`, which is being
    /// deleted: each socket closes, and each live connection acting as one
    /// is told `AUTHORIZATION_REVOKED` and closed (`workload-auth.md` §7).
    /// The connections' own teardown releases what they held. Returns the
    /// connections revoked.
    pub(crate) fn revoke_session_delegations(&mut self, session: SessionId) -> Vec<ClientId> {
        let mut revoked = Vec::new();
        for id in self.delegations.for_session(session) {
            let clients = self.withdraw_delegation(&id);
            tracing::info!(
                credential = %id,
                connections = clients.len(),
                "delegated agent credential revoked with its session"
            );
            for client in clients {
                if self.connection_revoked(client) {
                    continue;
                }
                self.mark_connection_revoked(client, crate::policy::Revocation::Revoked);
                if let Some(token) = self.client_connection_cancellation(client) {
                    token.cancel();
                }
                revoked.push(client);
            }
        }
        revoked
    }

    /// Record the cancellation that closes delegated credential `id`'s
    /// socket. `false` when `id` was withdrawn meanwhile.
    pub(crate) fn attach_delegation_listener(
        &mut self,
        id: &str,
        path: std::path::PathBuf,
        fd: std::os::fd::RawFd,
        close: tokio_util::sync::CancellationToken,
    ) -> bool {
        self.delegations.attach_listener(id, path, fd, close)
    }

    /// Every live delegated credential with an open socket, as a hot
    /// upgrade carries it.
    #[must_use]
    pub fn carried_delegations(&self, now: DateTime<Utc>) -> Vec<CarriedDelegation> {
        self.delegations.carried(now)
    }

    /// Reinstate a delegated credential a hot upgrade carried, if it still
    /// stands at `now`: it is well formed and unexpired, its session
    /// survived under the same wire id, and both the human it acts for and
    /// the principal that minted it still hold a credential (a delegate's
    /// own delegator must already be reinstated). Its socket is the
    /// caller's to adopt.
    ///
    /// # Errors
    ///
    /// The [`UnrestoredDelegation`]; nothing was reinstated.
    pub fn restore_delegation(
        &mut self,
        carried: &DelegationBlob,
        now: DateTime<Utc>,
    ) -> Result<DelegatedCredential, UnrestoredDelegation> {
        let verbs = Verbs::of(
            &Verb::ALL
                .into_iter()
                .filter(|verb| carried.verbs & verb.bit() != 0)
                .collect::<Vec<_>>(),
        );
        let agent_only = Verb::ALL
            .into_iter()
            .all(|verb| !verbs.contains(verb) || AGENT_VERBS.contains(verb));
        if !crate::delegation::is_delegated_id(&carried.id)
            || verbs.is_empty()
            || verbs.bits() != carried.verbs
            || !agent_only
        {
            return Err(UnrestoredDelegation::Malformed);
        }
        let expires_at = DateTime::from_timestamp_millis(carried.expires_at_ms)
            .ok_or(UnrestoredDelegation::Malformed)?;
        if expires_at <= now {
            return Err(UnrestoredDelegation::Expired);
        }
        let session = self
            .idspace
            .resolve_session(phux_protocol::ids::SessionId::new(carried.group))
            .filter(|session| self.registry().session(*session).is_some())
            .ok_or(UnrestoredDelegation::NoSession)?;
        if !self.principal_holds_credential(&carried.on_behalf_of, now)
            || !self.principal_holds_credential(&carried.delegator, now)
        {
            return Err(UnrestoredDelegation::NoLauncher);
        }
        let credential = DelegatedCredential {
            id: carried.id.clone(),
            session,
            group: carried.group,
            verbs,
            on_behalf_of: carried.on_behalf_of.clone(),
            delegator: carried.delegator.clone(),
            expires_at,
        };
        self.delegations.insert(credential.clone());
        for operation in &carried.operations {
            self.delegations.note_operation(*operation, &credential.id);
        }
        Ok(credential)
    }

    /// Whether `principal` still holds a credential at `now`: the owner
    /// always does; a delegate while its credential is live; a registry
    /// credential while the registry admits it.
    fn principal_holds_credential(&self, principal: &PrincipalId, now: DateTime<Utc>) -> bool {
        match principal {
            PrincipalId::Owner => true,
            PrincipalId::System => false,
            PrincipalId::Credential(id) if crate::delegation::is_delegated_id(id) => {
                self.delegations.live(id, now).is_some()
            }
            PrincipalId::Credential(id) => self
                .policy_engine()
                .workload_registry()
                .is_some_and(|registry| registry.current().lookup(id).is_some()),
        }
    }

    /// The socket delegated credential `id`'s connections arrive on.
    #[must_use]
    pub fn delegation_socket(&self, id: &str) -> Option<&std::path::Path> {
        self.delegations.socket_path(id)
    }

    /// Remember that `operation` minted delegated credential `id`, so a
    /// retry answers it.
    pub(crate) fn note_delegation_operation(&mut self, operation: [u8; 16], id: &str) {
        self.delegations.note_operation(operation, id);
    }

    /// The live delegated credential `operation` minted, if any.
    #[must_use]
    pub fn delegation_minted_by(
        &self,
        operation: &[u8; 16],
        now: DateTime<Utc>,
    ) -> Option<&DelegatedCredential> {
        let id = self.delegations.minted_by(operation)?;
        self.delegations.live(id, now)
    }

    /// Every live delegated credential scoped to `session`.
    #[must_use]
    pub fn delegations_for_session(&self, session: SessionId) -> Vec<String> {
        self.delegations.for_session(session)
    }
}
