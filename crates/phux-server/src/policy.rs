//! The server's authorization: one grant minted per connection at HELLO, and
//! the dispatch guard that enforces it (`docs/spec/workload-auth.md` §5-§8,
//! ADR-0116, ADR-0125).
//!
//! # The grant
//!
//! [`PolicyEngine::authorize_hello`] runs once per connection, with the
//! identity the accepting transport authenticated, and returns the
//! [`ConnectionGrant`] the connection keeps for its lifetime. There are two
//! shapes of authority:
//!
//! - [`Authority::Owner`]: all six verbs at Global, minted for every
//!   connection in the `local` and transitional postures and for the owner's
//!   Unix socket in `paired`. The dispatch guard admits every frame for it,
//!   so the existing handlers (and their domain checks, such as `SHUTDOWN`'s
//!   owner-socket rule) behave exactly as they did before this module
//!   enforced anything.
//! - [`Authority::Scoped`]: a workload's registry ceiling as a conjunctive
//!   [`EffectiveScopeSet`], minted only in `paired` and only from the live
//!   workload registry, never from a bearer token's recorded scopes.
//!
//! # The guard
//!
//! [`authorize_frame`], [`authorize_command`], and [`authorize_stream_bind`]
//! are the three entry points the client loop calls (after frame decode, at
//! the top of command dispatch above the input-lane and satellite-relay
//! branches, and at QUIC `STREAM_BIND`). All three share
//! [`enforce`], which reads the closed classification of
//! [`phux_protocol::kinds`] and resolves the subject against the same state
//! snapshot. There is no second table.
//!
//! # Engines
//!
//! [`PermissivePolicy`] mints [`Authority::Owner`] for everyone: the
//! transitional posture (no `[policy] mode`). [`ScopedPolicy`] implements
//! the two closed modes. `ServerConfig::policy_engine` still overrides the
//! choice for tests and embedders (ADR-0072).

mod enforce;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::scope::{EffectiveScopeSet, ScopeError, ScopeGrant, TerminalScopeSet};
use phux_protocol::wire::frame::DetachReason;
use tracing::debug;

use crate::auth::AuthenticatedCredential;
use crate::workload::ReloadingWorkloadRegistry;

pub use enforce::{
    Denial, Request, authorize_command, authorize_frame, authorize_stream_bind, enforce,
    resolve_attach_session,
};

/// The tracing target every authorization decision logs under.
pub const POLICY_TARGET: &str = "phux_server::policy";

/// At most one uncorrelated `PERMISSION_DENIED` per this interval per
/// connection (`workload-auth.md` §7). Denied frames past the limit are
/// still dropped; only the error is suppressed.
pub const DENIAL_ERROR_INTERVAL: Duration = Duration::from_secs(1);

/// Why a live connection's authority was withdrawn (`workload-auth.md` §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revocation {
    /// The credential was removed or revoked, its store stopped loading, or
    /// the ceiling no longer contains the minted grant.
    Revoked,
    /// The credential reached its expiry.
    Expired,
}

impl Revocation {
    /// The `DETACHED` reason that ends the connection.
    #[must_use]
    pub const fn detach_reason(self) -> DetachReason {
        match self {
            Self::Revoked => DetachReason::AuthorizationRevoked,
            Self::Expired => DetachReason::AuthorizationExpired,
        }
    }

    /// The diagnostic text on the goodbye frames. Names no credential and
    /// no rule.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Revoked => "authorization revoked",
            Self::Expired => "authorization expired",
        }
    }
}

/// What a connection's writer owes the peer once its authority is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goodbye {
    /// `ERROR { PERMISSION_DENIED }`, then `DETACHED` with the revocation's
    /// reason, then close.
    Announce(Revocation),
    /// Close with no frame: the connection never completed HELLO, so there
    /// is no attach for a `DETACHED` to end (`workload-auth.md` §7).
    Silent,
}

/// The registry state a live scoped grant was last confirmed against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryStamp {
    /// The registry file instance.
    pub instance: Option<String>,
    /// The registry generation.
    pub generation: u64,
    /// The credential's expiry in that state.
    pub expires_at: Option<DateTime<Utc>>,
}

/// One live connection as the revocation watcher sees it.
#[derive(Debug, Clone)]
pub(crate) struct WatchedConnection {
    /// The connection.
    pub(crate) client: crate::state::ClientId,
    /// Its grant, as it stood when the watcher looked.
    pub(crate) grant: ConnectionGrant,
    /// The pairing-store admission, if a bearer token admitted it.
    pub(crate) bearer: Option<crate::auth::BearerAdmission>,
}

/// What a connection may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authority {
    /// All six verbs at Global, with the handlers' own domain checks as the
    /// only limit: the pre-enforcement behaviour, byte for byte.
    Owner,
    /// A workload's grant, enforced at dispatch.
    Scoped {
        /// The grants the connection holds, as `phux.whoami/v1` reports them.
        /// HELLO carries no scope request, so this is the registry ceiling.
        granted: TerminalScopeSet,
        /// `granted` intersected with the ceiling, as conjunctive clauses.
        effective: EffectiveScopeSet,
    },
}

impl Authority {
    /// The grants to report for this authority: all six verbs at Global for
    /// the owner, the minted set otherwise.
    #[must_use]
    pub fn reported_grants(&self) -> Vec<ScopeGrant> {
        match self {
            Self::Owner => TerminalScopeSet::all_global().grants().to_vec(),
            Self::Scoped { granted, .. } => granted.grants().to_vec(),
        }
    }
}

/// The authority minted for one connection at HELLO, retained for its
/// lifetime (`workload-auth.md` §7): the credential it came from, the
/// registry state that minted it, and its expiry.
#[derive(Debug, Clone)]
pub struct ConnectionGrant {
    /// What the connection may do.
    pub authority: Authority,
    /// The workload credential id, for a scoped grant.
    pub credential_id: Option<String>,
    /// The registry file instance that minted a scoped grant.
    pub registry_instance: Option<String>,
    /// The registry generation that minted a scoped grant.
    pub registry_generation: u64,
    /// When a scoped grant's credential expires, if it does.
    pub expires_at: Option<DateTime<Utc>>,
    /// When this connection was last sent an uncorrelated denial.
    last_denial_error: Option<Instant>,
    /// Set once the connection's authority was withdrawn while it was live;
    /// every guard then denies everything (`workload-auth.md` §7 step 1).
    revoked: Option<Revocation>,
}

impl ConnectionGrant {
    /// The owner's grant.
    #[must_use]
    pub const fn owner() -> Self {
        Self {
            authority: Authority::Owner,
            credential_id: None,
            registry_instance: None,
            registry_generation: 0,
            expires_at: None,
            last_denial_error: None,
            revoked: None,
        }
    }

    /// A scoped grant of `granted` with no requested attenuation: one
    /// clause per grant (`workload-auth.md` §5.1), bound to `credential_id`.
    ///
    /// # Errors
    ///
    /// [`ScopeError::TooLarge`] when the effective set would encode larger
    /// than §5 allows.
    pub fn scoped(
        granted: TerminalScopeSet,
        credential_id: Option<String>,
    ) -> Result<Self, ScopeError> {
        let effective = EffectiveScopeSet::unattenuated(&granted)?;
        Ok(Self {
            authority: Authority::Scoped { granted, effective },
            credential_id,
            registry_instance: None,
            registry_generation: 0,
            expires_at: None,
            last_denial_error: None,
            revoked: None,
        })
    }

    /// Whether this is the owner's grant.
    #[must_use]
    pub const fn is_owner(&self) -> bool {
        matches!(self.authority, Authority::Owner)
    }

    /// Whether the credential behind this grant has expired at `now`.
    #[must_use]
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|expiry| expiry <= now)
    }

    /// Whether an uncorrelated denial error may be sent now; records the
    /// send when it may. At most one per [`DENIAL_ERROR_INTERVAL`].
    pub fn admit_denial_error(&mut self, now: Instant) -> bool {
        let recent = self
            .last_denial_error
            .is_some_and(|last| now.saturating_duration_since(last) < DENIAL_ERROR_INTERVAL);
        if recent {
            return false;
        }
        self.last_denial_error = Some(now);
        true
    }

    /// The owner-shaped placeholder for a connection revoked before its
    /// HELLO minted anything: it admits nothing, and a grant minted later
    /// never replaces it.
    #[must_use]
    pub(crate) const fn revoked_placeholder(revocation: Revocation) -> Self {
        let mut grant = Self::owner();
        grant.revoked = Some(revocation);
        grant
    }

    /// Why the connection's authority was withdrawn, once it was.
    #[must_use]
    pub const fn revocation(&self) -> Option<Revocation> {
        self.revoked
    }

    /// Withdraw the grant. The first cause sticks.
    pub(crate) const fn revoke(&mut self, revocation: Revocation) {
        if self.revoked.is_none() {
            self.revoked = Some(revocation);
        }
    }

    /// Adopt a newer registry state that still contains every minted
    /// clause: the clauses stay as minted; the stamp and the expiry follow
    /// the registry.
    pub(crate) fn refresh(&mut self, stamp: RegistryStamp) {
        self.registry_instance = stamp.instance;
        self.registry_generation = stamp.generation;
        self.expires_at = stamp.expires_at;
    }
}

/// The boxed future [`PolicyEngine::authorize_hello`] returns.
pub type GrantFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ConnectionGrant, PolicyError>> + Send + 'a>>;

/// The server's HELLO authorization decision.
///
/// Consulted once per connection, after the transport authenticated the
/// peer and before any other frame is processed. An `Err` refuses the
/// handshake; an `Ok` grant is retained and enforced at every dispatch.
///
/// `&self` so the implementation can be shared across tasks (it is held as
/// `Arc<dyn PolicyEngine>`), and the method returns a boxed future so the
/// trait stays object-safe.
pub trait PolicyEngine: Send + Sync + std::fmt::Debug {
    /// Mint the grant for a connection from its transport identity and the
    /// credential the transport verified, if any.
    fn authorize_hello<'a>(
        &'a self,
        peer_identity: &'a PeerIdentity,
        credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a>;

    /// The live workload registry this engine mints scoped grants from, so
    /// the revocation watcher can re-judge them (`workload-auth.md` §7).
    /// `None` for an engine that reads no registry: only expiry then ends
    /// its grants.
    fn workload_registry(&self) -> Option<Arc<ReloadingWorkloadRegistry>> {
        None
    }
}

/// The transitional engine: every admitted connection holds the owner's
/// grant, exactly as before scope enforcement existed. It runs when no
/// `[policy] mode` is configured.
#[derive(Debug, Clone, Copy)]
pub struct PermissivePolicy;

impl PermissivePolicy {
    /// Shared instance (stateless).
    pub const INSTANCE: Self = Self;
}

impl PolicyEngine for PermissivePolicy {
    fn authorize_hello<'a>(
        &'a self,
        _peer_identity: &'a PeerIdentity,
        _credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        Box::pin(async move { Ok(ConnectionGrant::owner()) })
    }
}

/// The engine for the two closed modes (`workload-auth.md` §8).
///
/// Both give the owner's Unix socket the owner's grant. `local` refuses
/// every other transport. `paired` admits a TLS connection only with a
/// credential the live workload registry holds as active, and mints that
/// credential's ceiling; everything else is refused.
#[derive(Debug, Clone)]
pub struct ScopedPolicy {
    registry: Option<Arc<ReloadingWorkloadRegistry>>,
}

impl ScopedPolicy {
    /// The `local` mode: the owner's socket only.
    #[must_use]
    pub const fn local() -> Self {
        Self { registry: None }
    }

    /// The `paired` mode, minting scoped grants from `registry`.
    #[must_use]
    pub const fn paired(registry: Arc<ReloadingWorkloadRegistry>) -> Self {
        Self {
            registry: Some(registry),
        }
    }

    fn decide(
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
        scoped_grant(registry, credential)
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
/// the serving user's uid (`workload-auth.md` §3: kernel-uid authority).
fn is_owner_socket(peer: &PeerIdentity) -> bool {
    matches!(peer.transport, TransportType::UnixSocket)
        && peer.uid == nix::unistd::geteuid().as_raw()
}

/// Mint a scoped grant from the registry snapshot current right now. The
/// ceiling is read from the registry record, never from the credential the
/// transport cached, so a bearer credential's recorded scopes can never
/// become authority and a record revoked since admission mints nothing.
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
    let mut grant = ConnectionGrant::scoped(ceiling, Some(record.id.clone())).map_err(|_| {
        debug!(target: POLICY_TARGET, credential = %credential.id, "HELLO refused: effective set over the §5 bound");
        PolicyError::refused()
    })?;
    grant.registry_instance = snapshot.instance_id().map(str::to_owned);
    grant.registry_generation = snapshot.generation();
    grant.expires_at = record
        .expires_at
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0));
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
    /// reveal which test failed (`workload-auth.md` §7).
    fn refused() -> Self {
        Self::Unauthorized("not authorized".to_owned())
    }
}

// -----------------------------------------------------------------------------
// Posture: which engine a server runs, decided at startup.
// -----------------------------------------------------------------------------

/// The authorization posture a server starts in (`workload-auth.md` §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyPosture {
    /// No `[policy] mode`: every admitted connection holds the owner's grant.
    /// A remote listener is kept (and warned about) until workload mTLS
    /// covers every remote transport (PHA-406 decision H1).
    Transitional {
        /// Whether a remote listener or connector is configured, so the
        /// server warns once at startup.
        remote_listener: bool,
    },
    /// `mode = "local"`: the owner's socket only.
    Local,
    /// `mode = "paired"`, or `PHUX_WORKLOAD_MTLS` with no mode: workload
    /// mTLS on every TLS transport, scoped grants at dispatch.
    Paired,
}

/// A `[policy]` setting the server refuses to start with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PostureError {
    /// `mode = "local"` beside `PHUX_WORKLOAD_MTLS`.
    #[error(
        "[policy] mode = \"local\" conflicts with PHUX_WORKLOAD_MTLS: set mode = \"paired\" or unset PHUX_WORKLOAD_MTLS"
    )]
    LocalWithWorkloadMtls,
    /// `mode = "local"` beside a configured remote listener or connector.
    #[error(
        "[policy] mode = \"local\" admits the owner socket only, but a remote listener or relay connector is configured: remove it (--listen, --quic, --webtransport, PHUX_WS_ADDR, PHUX_QUIC_ADDR, PHUX_WT_ADDR, [[connector]]) or set mode = \"paired\""
    )]
    LocalWithRemoteListener,
}

impl PolicyPosture {
    /// Decide the posture from the configured mode, whether
    /// `PHUX_WORKLOAD_MTLS` is set, and whether a remote listener or relay
    /// connector is configured.
    ///
    /// # Errors
    ///
    /// A [`PostureError`] for a `local` mode the rest of the configuration
    /// contradicts.
    pub const fn resolve(
        mode: Option<phux_config::PolicyMode>,
        workload_mtls_env: bool,
        remote_listener: bool,
    ) -> Result<Self, PostureError> {
        match mode {
            Some(phux_config::PolicyMode::Paired) => Ok(Self::Paired),
            Some(phux_config::PolicyMode::Local) => Self::local(workload_mtls_env, remote_listener),
            None if workload_mtls_env => Ok(Self::Paired),
            None => Ok(Self::Transitional { remote_listener }),
        }
    }

    const fn local(workload_mtls_env: bool, remote_listener: bool) -> Result<Self, PostureError> {
        if workload_mtls_env {
            return Err(PostureError::LocalWithWorkloadMtls);
        }
        if remote_listener {
            return Err(PostureError::LocalWithRemoteListener);
        }
        Ok(Self::Local)
    }

    /// Whether TLS listeners must require a workload client certificate.
    #[must_use]
    pub const fn requires_workload_mtls(self) -> bool {
        matches!(self, Self::Paired)
    }

    /// Whether the server admits the owner's socket only: no remote door,
    /// configured, auto-bound, or opened on demand.
    #[must_use]
    pub const fn is_local(self) -> bool {
        matches!(self, Self::Local)
    }

    /// Whether every admitted connection holds the owner's grant.
    #[must_use]
    pub const fn is_transitional(self) -> bool {
        matches!(self, Self::Transitional { .. })
    }

    /// Whether the server should warn at startup that remote consumers keep
    /// the owner's grant (the transitional posture with a remote listener).
    #[must_use]
    pub const fn warns_remote_owner_grant(self) -> bool {
        matches!(
            self,
            Self::Transitional {
                remote_listener: true
            }
        )
    }
}
