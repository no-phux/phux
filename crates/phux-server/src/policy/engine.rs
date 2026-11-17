//! The HELLO policy seam: [`PolicyEngine`] and the closed-mode
//! [`ScopedPolicy`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use jiff::Timestamp;
use phux_protocol::policy::{PeerIdentity, TransportType};
use tracing::debug;

use super::{ConnectionGrant, POLICY_TARGET};
use crate::auth::AuthenticatedCredential;
use crate::workload::ReloadingWorkloadRegistry;

/// The boxed future [`PolicyEngine::authorize_hello`] returns.
pub type GrantFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ConnectionGrant, PolicyError>> + Send + 'a>>;

/// The HELLO authorization decision, consulted once per connection after
/// transport authentication. Object-safe (`Arc<dyn PolicyEngine>`).
pub trait PolicyEngine: Send + Sync + std::fmt::Debug {
    /// Mint the grant for a connection from its transport identity and the
    /// credential the transport verified, if any.
    fn authorize_hello<'a>(
        &'a self,
        peer_identity: &'a PeerIdentity,
        credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a>;

    /// The registry scoped grants come from, for the revocation watcher;
    /// `None` when none is read.
    fn workload_registry(&self) -> Option<Arc<ReloadingWorkloadRegistry>> {
        None
    }
}

/// The closed-mode engine (§8): the owner socket gets the owner's grant;
/// `local` refuses everything else; `paired` admits TLS connections with an
/// active registry credential and mints its ceiling.
#[derive(Debug, Clone)]
pub struct ScopedPolicy {
    registry: Option<Arc<ReloadingWorkloadRegistry>>,
    /// `[policy] tailnet-owner`: grant the owner's own tailnet devices a
    /// paired person's device ceiling.
    tailnet_owner: bool,
}

impl ScopedPolicy {
    /// The `local` mode: the owner's socket only.
    #[must_use]
    pub const fn local() -> Self {
        Self {
            registry: None,
            tailnet_owner: false,
        }
    }

    /// The `paired` mode, minting scoped grants from `registry`.
    #[must_use]
    pub const fn paired(registry: Arc<ReloadingWorkloadRegistry>) -> Self {
        Self {
            registry: Some(registry),
            tailnet_owner: false,
        }
    }

    /// Also grant a QUIC connection the admission vouched for as the owner's
    /// own tailnet device (`docs/config.md#tailnet-owner-devices`) what a
    /// person's paired device holds.
    #[must_use]
    pub const fn with_tailnet_owner(mut self, on: bool) -> Self {
        self.tailnet_owner = on;
        self
    }

    pub(super) fn decide(
        &self,
        peer: &PeerIdentity,
        credential: Option<&AuthenticatedCredential>,
    ) -> Result<ConnectionGrant, PolicyError> {
        if is_owner_socket(peer) {
            return Ok(ConnectionGrant::owner());
        }
        let Some(registry) = &self.registry else {
            debug!(target: POLICY_TARGET, transport = ?peer.transport, "HELLO refused: local policy admits the owner socket only");
            return Err(PolicyError::refused());
        };
        let Some(credential) = credential else {
            debug!(target: POLICY_TARGET, transport = ?peer.transport, "HELLO refused: no workload credential");
            return Err(PolicyError::refused());
        };
        if crate::auth::is_tailnet_owner(credential) {
            return self.tailnet_owner_grant(peer, credential);
        }
        scoped_grant(registry, credential)
    }

    /// A paired person's device ceiling for an owner device, attributed to
    /// its `tailnet:` credential; only over QUIC and only with the mode on.
    fn tailnet_owner_grant(
        &self,
        peer: &PeerIdentity,
        credential: &AuthenticatedCredential,
    ) -> Result<ConnectionGrant, PolicyError> {
        if !self.tailnet_owner || peer.transport != TransportType::Quic {
            debug!(target: POLICY_TARGET, "HELLO refused: tailnet owner devices are not admitted");
            return Err(PolicyError::refused());
        }
        let scopes = crate::workload::enrolled_scopes(
            vec![crate::workload::PAIRED_DEVICE_SCOPE.to_owned()],
            Some(crate::workload::EnrolledFor::Human),
        );
        let ceiling = phux_protocol::scope::TerminalScopeSet::parse_all(&scopes)
            .map_err(|_| PolicyError::Internal("paired device ceiling".to_owned()))?;
        ConnectionGrant::scoped(ceiling, None)
            .map(|grant| {
                grant
                    .with_principal(phux_core::PrincipalId::Credential(credential.id.clone()))
                    .with_kind(phux_core::actor::PrincipalKind::Human)
            })
            .map_err(|_| PolicyError::Internal("paired device ceiling".to_owned()))
    }
}

impl PolicyEngine for ScopedPolicy {
    fn authorize_hello<'a>(
        &'a self,
        peer_identity: &'a PeerIdentity,
        credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        Box::pin(async move { self.decide(peer_identity, credential) })
    }

    fn workload_registry(&self) -> Option<Arc<ReloadingWorkloadRegistry>> {
        self.registry.clone()
    }
}

/// The owner's transport is the kernel-authenticated Unix socket, carrying
/// the serving user's uid (`docs/remote.md#who-can-connect`: kernel-uid authority).
fn is_owner_socket(peer: &PeerIdentity) -> bool {
    matches!(peer.transport, TransportType::UnixSocket)
        && peer.uid == nix::unistd::geteuid().as_raw()
}

/// Mint a scoped grant from the current registry record (never from the
/// transport's cached credential), so a revoked record mints nothing.
fn scoped_grant(
    registry: &ReloadingWorkloadRegistry,
    credential: &AuthenticatedCredential,
) -> Result<ConnectionGrant, PolicyError> {
    if !crate::workload::is_canonical_credential_id(&credential.id) {
        debug!(target: POLICY_TARGET, "HELLO refused: credential is not a workload credential");
        return Err(PolicyError::refused());
    }
    let snapshot = registry.current();
    let Some(record) = snapshot.lookup(&credential.id) else {
        debug!(target: POLICY_TARGET, credential = %credential.id, "HELLO refused: credential not active in the registry");
        return Err(PolicyError::refused());
    };
    let ceiling = record.ceiling().map_err(|_| {
        debug!(target: POLICY_TARGET, credential = %credential.id, "HELLO refused: ceiling does not parse");
        PolicyError::refused()
    })?;
    // HELLO carries no scope request (workload-auth §5), so the request is
    // the ceiling: one clause per ceiling grant (§5.1).
    let mut grant = ConnectionGrant::scoped(ceiling, Some(record.id.clone()))
        .map_err(|_| {
            debug!(target: POLICY_TARGET, credential = %credential.id, "HELLO refused: effective set over the §5 bound");
            PolicyError::refused()
        })?
        // A paired device is a person; an automation is not (ADR-0163 §6).
        .with_kind(record.enrolled_for().kind());
    grant.registry_instance = snapshot.instance_id().map(str::to_owned);
    grant.registry_generation = snapshot.generation();
    grant.expires_at = record
        .expires_at
        .and_then(|seconds| Timestamp::from_second(seconds).ok());
    Ok(grant)
}

/// Errors from the policy seam. An `Err` at HELLO is a refusal.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    /// The consumer is not permitted to connect.
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    /// The policy engine itself failed. Fails closed, like a denial: the
    /// server must not admit a peer it could not evaluate.
    #[error("internal: {0}")]
    Internal(String),
}

impl PolicyError {
    /// The one refusal every failed check returns: diagnostics must not
    /// reveal which test failed (`docs/remote.md#managing-credentials`).
    fn refused() -> Self {
        Self::Unauthorized("not authorized".to_owned())
    }
}
