//! A connection's grant: its [`Authority`] and the session points a scope
//! selector names.

use std::time::Instant;

use chrono::{DateTime, Utc};
use phux_core::PrincipalId;
use phux_core::actor::PrincipalKind;
use phux_protocol::kinds::Verb;
use phux_protocol::scope::{EffectiveScopeSet, ScopeError, ScopeGrant, Selector, TerminalScopeSet};

use super::{DENIAL_ERROR_INTERVAL, RegistryStamp, Revocation};

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
