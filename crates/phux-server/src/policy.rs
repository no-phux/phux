//! The server's authorization (`docs/spec/workload-auth.md` §5-§8, ADR-0116,
//! ADR-0125): one grant per connection, minted at HELLO, enforced at every
//! dispatch.
//!
//! [`Authority::Owner`] (all verbs at Global) is minted for the owner's
//! socket in both modes, so its handlers behave as before.
//! [`Authority::Scoped`] is a workload's registry ceiling, minted only in
//! `paired` from the live registry, never from bearer scopes. There is no
//! third mode (ADR-0179): nothing grants a remote peer the owner's authority.
//!
//! [`authorize_frame`], [`authorize_command`], and [`authorize_stream_bind`]
//! share [`enforce`] over [`phux_protocol::kinds`]'s classification.
//! [`ScopedPolicy`] is the engine of both modes; an embedder may inject its
//! own through `ServerConfig::policy_engine` (ADR-0072).

mod enforce;
pub mod filter;
mod hold;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use phux_core::PrincipalId;
use phux_core::actor::PrincipalKind;
use phux_protocol::kinds::Verb;
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::scope::{EffectiveScopeSet, ScopeError, ScopeGrant, Selector, TerminalScopeSet};
use phux_protocol::wire::frame::DetachReason;
use tracing::debug;

use crate::auth::AuthenticatedCredential;
use crate::workload::ReloadingWorkloadRegistry;

pub use enforce::{
    Denial, Request, authorize_command, authorize_frame, authorize_stream_bind, enforce,
    resolve_attach_session,
};
pub use hold::Admission;

/// The Terminals a batch command (`KILL_RESOURCES`, `CLOSE_TAB_RESOURCES`)
/// names; the guard, holds, and withdrawal all read this list.
#[must_use]
pub(crate) fn batch_terminals(
    command: &phux_protocol::wire::frame::Command,
) -> &[phux_protocol::ids::ResourceId] {
    use phux_protocol::wire::frame::Command;
    match command {
        Command::KillResources { ids, .. } | Command::CloseTabResources { ids } => ids,
        _ => &[],
    }
}

/// Every Terminal a held command names (ADR-0128).
pub(crate) fn held_terminals(
    command: &phux_protocol::wire::frame::Command,
) -> impl Iterator<Item = &phux_protocol::ids::ResourceId> {
    batch_terminals(command)
        .iter()
        .chain(enforce::named_terminal(Request::Command(command)))
}

/// The tracing target every authorization decision logs under.
pub const POLICY_TARGET: &str = "phux_server::policy";

/// At most one uncorrelated `PERMISSION_DENIED` per connection per interval
/// (§7); denied frames are still dropped.
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

/// The authority minted for one connection at HELLO: its credential,
/// registry state, and expiry.
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
    /// The authenticated principal every action on this connection is
    /// attributed to (ADR-0163).
    principal: PrincipalId,
    /// The human whose grant a delegated principal exercises.
    on_behalf_of: Option<PrincipalId>,
    /// Whether the principal is a person, an agent, or an automation: what
    /// input-lease precedence turns on (ADR-0163 §6).
    kind: PrincipalKind,
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
            principal: PrincipalId::Owner,
            on_behalf_of: None,
            kind: PrincipalKind::Human,
        }
    }

    /// A scoped grant of `granted` with no requested attenuation: one
    /// clause per grant (`workload-auth.md` §5.1), bound to `credential_id`.
    /// Its principal is that credential ([`WORKLOAD_PRINCIPAL`] when none is
    /// named), an automation until [`Self::with_kind`] says otherwise.
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
        let principal = PrincipalId::Credential(
            credential_id
                .clone()
                .unwrap_or_else(|| WORKLOAD_PRINCIPAL.to_owned()),
        );
        Ok(Self {
            authority: Authority::Scoped { granted, effective },
            credential_id,
            registry_instance: None,
            registry_generation: 0,
            expires_at: None,
            last_denial_error: None,
            revoked: None,
            principal,
            on_behalf_of: None,
            kind: PrincipalKind::Automation,
        })
    }

    /// This grant, acting as a principal of `kind`: how a paired device's
    /// credential becomes a human principal.
    #[must_use]
    pub const fn with_kind(mut self, kind: PrincipalKind) -> Self {
        self.kind = kind;
        self
    }

    /// A delegated credential's grant (ADR-0163): `granted` exercised by
    /// `principal` on behalf of `on_behalf_of`, lapsing at `expires_at`.
    /// It names no registry credential, so the revocation watcher judges
    /// only its expiry; withdrawing the delegation revokes it directly.
    ///
    /// # Errors
    ///
    /// As [`Self::scoped`].
    pub fn delegated(
        granted: TerminalScopeSet,
        principal: PrincipalId,
        on_behalf_of: PrincipalId,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, ScopeError> {
        let mut grant = Self::scoped(granted, None)?;
        grant.principal = principal;
        grant.on_behalf_of = Some(on_behalf_of);
        grant.expires_at = Some(expires_at);
        grant.kind = PrincipalKind::Agent;
        Ok(grant)
    }

    /// What kind of party the principal is.
    #[must_use]
    pub const fn kind(&self) -> PrincipalKind {
        self.kind
    }

    /// The authenticated principal this connection acts as.
    #[must_use]
    pub const fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    /// The human a delegated principal acts for.
    #[must_use]
    pub const fn on_behalf_of(&self) -> Option<&PrincipalId> {
        self.on_behalf_of.as_ref()
    }

    /// The human this connection ultimately answers to: the delegator for a
    /// delegated principal, else the principal itself.
    #[must_use]
    pub const fn answering_principal(&self) -> &PrincipalId {
        match &self.on_behalf_of {
            Some(human) => human,
            None => &self.principal,
        }
    }

    /// Whether this grant admits `verb` on every resource of the local
    /// session `group`: the owner always does; a scoped grant needs an
    /// un-held clause carrying `verb` on a selector that contains the whole
    /// session (Global, the local host, or that Group by id or key).
    #[must_use]
    pub fn admits_session(&self, verb: Verb, group: GroupPoint) -> bool {
        if self.revoked.is_some() {
            return false;
        }
        match &self.authority {
            Authority::Owner => true,
            Authority::Scoped { effective, .. } => effective.clauses().iter().any(|clause| {
                clause.verbs.contains(verb)
                    && !clause.held.contains(verb)
                    && [&clause.ceiling, &clause.requested]
                        .into_iter()
                        .all(|selector| contains_session(selector, group))
            }),
        }
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

    /// The placeholder for a connection revoked before its HELLO minted
    /// anything: admits nothing and is never replaced.
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

    /// Adopt a newer registry state that keeps every minted clause (stamp and
    /// expiry follow the registry).
    pub(crate) fn refresh(&mut self, stamp: RegistryStamp) {
        self.registry_instance = stamp.instance;
        self.registry_generation = stamp.generation;
        self.expires_at = stamp.expires_at;
    }
}

/// The principal name of a scoped grant minted with no credential id.
pub const WORKLOAD_PRINCIPAL: &str = "workload";

/// Whether `selector` covers every resource of the session `group`.
fn contains_session(selector: &Selector, group: GroupPoint) -> bool {
    matches!(selector, Selector::Global | Selector::HostLocal) || group.is_in(selector)
}

/// The persistent key of `session`'s record, as the wire spells it.
fn session_key(
    s: &crate::state::ServerState,
    session: phux_core::ids::SessionId,
) -> Option<phux_protocol::ids::SessionKey> {
    s.registry()
        .session_key(session)
        .and_then(crate::org_wire::session_key)
}

/// A local Group (session) as a scope subject: its wire session id, which a
/// `group:` selector names, and its record's persistent key, which a
/// `session:` selector names (workload-auth §5.1).
///
/// The two are independent: a session gets its wire id lazily but its
/// record key at creation, so a
/// `session:` grant reaches it before anything has named it on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupPoint {
    /// The wire session id; `None` until something interns one for it.
    pub id: Option<u32>,
    /// The session record's key; `None` only for a session with no record.
    pub key: Option<phux_protocol::ids::SessionKey>,
}

impl GroupPoint {
    /// The session `session` as it stands in `s`; `None` only when it has
    /// neither a wire id nor a record.
    #[must_use]
    pub fn of(s: &crate::state::ServerState, session: phux_core::ids::SessionId) -> Option<Self> {
        let point = Self {
            id: s
                .idspace
                .session_wire(session)
                .map(phux_protocol::ids::SessionId::get),
            key: session_key(s, session),
        };
        (point.id.is_some() || point.key.is_some()).then_some(point)
    }

    /// The session `session` under the wire id `id` already interned for it.
    #[must_use]
    pub fn with_id(
        s: &crate::state::ServerState,
        session: phux_core::ids::SessionId,
        id: u32,
    ) -> Self {
        Self {
            id: Some(id),
            key: session_key(s, session),
        }
    }

    /// The session a snapshot names by wire id; its key is absent when no
    /// live session holds that id.
    #[must_use]
    pub fn of_wire(s: &crate::state::ServerState, id: phux_protocol::ids::SessionId) -> Self {
        Self {
            id: Some(id.get()),
            key: s
                .idspace
                .resolve_session(id)
                .and_then(|session| session_key(s, session)),
        }
    }

    /// Whether the `group:` or `session:` selector `selector` names this
    /// session; `false` for every other selector.
    #[must_use]
    pub fn is_in(self, selector: &Selector) -> bool {
        match selector {
            Selector::Group(id) => self.id == Some(*id),
            Selector::Session(key) => self.key == Some(*key),
            _ => false,
        }
    }
}

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

// --- Posture: which engine a server runs ---

/// The authorization posture a server starts in (`workload-auth.md` §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyPosture {
    /// `mode = "local"`, or no mode and nothing past the owner's socket: the
    /// owner's socket only.
    Local,
    /// `mode = "paired"`: workload mTLS on every TLS transport, scoped
    /// grants at dispatch.
    Paired,
    /// No mode, and the embedder injected its own engine through
    /// `ServerConfig::policy_engine` (ADR-0072): that engine decides every
    /// HELLO, and listeners do not require a workload certificate. `phux
    /// server` never injects an engine, so no configuration reaches this.
    Embedded,
}

/// A `[policy]` setting the server refuses to start with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PostureError {
    /// `PHUX_WORKLOAD_MTLS` is set. It was the opt-in before `[policy] mode`
    /// existed and is retired (ADR-0179); a server that still sees it says
    /// so rather than quietly choosing a mode for it.
    #[error(
        "PHUX_WORKLOAD_MTLS is retired: set [policy] mode = \"paired\" in config.toml and unset PHUX_WORKLOAD_MTLS"
    )]
    RetiredWorkloadMtlsEnv,
    /// `mode = "local"` beside a configured remote listener or connector.
    #[error(
        "[policy] mode = \"local\" admits the owner socket only, but a remote listener or relay connector is configured: remove it (--listen, --quic, --webtransport, PHUX_WS_ADDR, PHUX_QUIC_ADDR, PHUX_WT_ADDR, [[connector]]) or set mode = \"paired\""
    )]
    LocalWithRemoteListener,
    /// No mode beside a configured remote listener or connector: nothing
    /// would authorize the consumers it admits (ADR-0179 retired the owner
    /// grant they once got).
    #[error(
        "no [policy] mode is set, but a remote listener or relay connector is configured: set [policy] mode = \"paired\" in config.toml to serve enrolled clients, or remove it (--listen, --quic, --webtransport, PHUX_WS_ADDR, PHUX_QUIC_ADDR, PHUX_WT_ADDR, [[connector]])"
    )]
    UnsetWithRemoteListener,
    /// No mode beside a configured workload CA or registry location: the
    /// operator named workload authority but did not choose to enforce it.
    #[error(
        "no [policy] mode is set, but PHUX_WORKLOAD_CA, PHUX_WORKLOAD_CA_KEY, or PHUX_WORKLOAD_KEYS names a workload authority location: set [policy] mode = \"paired\" in config.toml to enforce it, or unset those variables"
    )]
    UnsetWithWorkloadAuthority,
}

/// What [`PolicyPosture::resolve`] decides from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent startup facts, each named"
)]
pub struct PostureInputs {
    /// `[policy] mode`.
    pub mode: Option<phux_config::PolicyMode>,
    /// Whether the retired `PHUX_WORKLOAD_MTLS` is set.
    pub retired_workload_mtls_env: bool,
    /// Whether a remote listener or relay connector is configured.
    pub remote_listener: bool,
    /// Whether a workload CA or registry location is configured
    /// (`PHUX_WORKLOAD_CA`, `PHUX_WORKLOAD_CA_KEY`, `PHUX_WORKLOAD_KEYS`).
    pub workload_authority_configured: bool,
    /// Whether the embedder injected its own engine (ADR-0072).
    pub embedder_engine: bool,
}

impl PolicyPosture {
    /// Decide the posture (`docs/spec/workload-auth.md` §8). With no mode
    /// the server is `local`, and anything that would reach past the owner's
    /// socket is a startup error rather than a door nothing authorizes.
    ///
    /// # Errors
    ///
    /// A [`PostureError`] for the retired `PHUX_WORKLOAD_MTLS`, a `local`
    /// mode the rest of the configuration contradicts, or no mode beside a
    /// remote entry point or a configured workload authority location.
    pub const fn resolve(inputs: PostureInputs) -> Result<Self, PostureError> {
        if inputs.retired_workload_mtls_env {
            return Err(PostureError::RetiredWorkloadMtlsEnv);
        }
        match inputs.mode {
            Some(phux_config::PolicyMode::Paired) => Ok(Self::Paired),
            Some(phux_config::PolicyMode::Local) if inputs.remote_listener => {
                Err(PostureError::LocalWithRemoteListener)
            }
            None if inputs.embedder_engine => Ok(Self::Embedded),
            None if inputs.workload_authority_configured => {
                Err(PostureError::UnsetWithWorkloadAuthority)
            }
            None if inputs.remote_listener => Err(PostureError::UnsetWithRemoteListener),
            Some(phux_config::PolicyMode::Local) | None => Ok(Self::Local),
        }
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
}

/// An embedder engine (ADR-0072) for this crate's transport tests: every
/// connection holds the owner's grant, which no `phux server` mode gives a
/// remote peer (ADR-0179). Tests of authorization use [`ScopedPolicy`].
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct OwnerForAll;

#[cfg(test)]
impl PolicyEngine for OwnerForAll {
    fn authorize_hello<'a>(
        &'a self,
        _peer_identity: &'a PeerIdentity,
        _credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        Box::pin(async { Ok(ConnectionGrant::owner()) })
    }
}
