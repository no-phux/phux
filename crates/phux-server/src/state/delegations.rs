//! Minting, admitting, and withdrawing delegated agent credentials
//! (ADR-0163 §5); the ledger and its rules are [`crate::delegation`].

use chrono::{DateTime, Utc};
use phux_core::ids::SessionId;
use phux_protocol::kinds::Verb;

use super::{ClientId, ServerState};
use crate::delegation::{DelegatedCredential, DelegationError, DelegationRequest, MAX_LIFETIME};
use crate::policy::ConnectionGrant;

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

    /// Record the cancellation that closes delegated credential `id`'s
    /// socket. `false` when `id` was withdrawn meanwhile.
    pub(crate) fn attach_delegation_listener(
        &mut self,
        id: &str,
        close: tokio_util::sync::CancellationToken,
    ) -> bool {
        self.delegations.attach_listener(id, close)
    }

    /// Every live delegated credential scoped to `session`.
    #[must_use]
    pub fn delegations_for_session(&self, session: SessionId) -> Vec<String> {
        self.delegations.for_session(session)
    }
}
