//! Shared sub-record payload types: attach targets (SPEC §13), viewport
//! info, L3 metadata scope (SPEC §7.4), and spawn/move results (SPEC §10.1).

use crate::ids::{
    ClientId, GroupId, IdempotencyKey, ResourceId, ResourceKind, ServerInstance, SessionId,
};

/// The connection that caused an event or a metadata change (ADR-0123).
///
/// Positional on the wire: `client: u32 || credential_id: optional<str> ||
/// client_name: optional<str>`. Through a federation hub the actor is the
/// hub's link.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ActorRef {
    /// The acting connection's wire client id; does not survive a reconnect.
    pub client: ClientId,
    /// The paired credential the connection authenticated with, if any.
    pub credential_id: Option<String>,
    /// The `HELLO.client_name` the connection announced, if any.
    pub client_name: Option<String>,
}

impl ActorRef {
    /// An actor known only by its connection id.
    #[must_use]
    pub const fn new(client: ClientId) -> Self {
        Self {
            client,
            credential_id: None,
            client_name: None,
        }
    }

    /// Builder setter for [`Self::credential_id`].
    #[must_use]
    pub fn with_credential_id(mut self, credential_id: Option<String>) -> Self {
        self.credential_id = credential_id;
        self
    }

    /// Builder setter for [`Self::client_name`].
    #[must_use]
    pub fn with_client_name(mut self, client_name: Option<String>) -> Self {
        self.client_name = client_name;
        self
    }
}

/// The journal stamp on an `EVENT` (fields 3-6, ADR-0123), present iff field
/// 3 (`seq`) is. Boxed on the frame to keep [`FrameKind`](super::FrameKind)
/// small.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EventStamp {
    /// Server-wide journal sequence: starts at 1, strictly increasing within
    /// one server incarnation (`HELLO_OK.server_id`), never wraps.
    pub seq: u64,
    /// Server wall-clock time the event was journaled, Unix milliseconds.
    pub ts_ms: u64,
    /// The connection that caused the event; `None` for server-driven ones.
    pub actor: Option<ActorRef>,
    /// The idempotency key of the operation that caused the event.
    pub operation_id: Option<IdempotencyKey>,
}

impl EventStamp {
    /// A stamp with no actor and no operation.
    #[must_use]
    pub const fn new(seq: u64, ts_ms: u64) -> Self {
        Self {
            seq,
            ts_ms,
            actor: None,
            operation_id: None,
        }
    }

    /// Builder setter for [`Self::actor`].
    #[must_use]
    pub fn with_actor(mut self, actor: Option<ActorRef>) -> Self {
        self.actor = actor;
        self
    }

    /// Builder setter for [`Self::operation_id`].
    #[must_use]
    pub const fn with_operation_id(mut self, operation_id: Option<IdempotencyKey>) -> Self {
        self.operation_id = operation_id;
        self
    }
}

/// The kind, binding, and facet of a `SPAWN_RESOURCE`: fields 11-17
/// (`docs/spec/L1.md` §1.2).
///
/// An all-default value encodes to no fields and decodes back as `None`.
/// The decoder enforces per-kind rules: an
/// [`AgentSession`](ResourceKind::AgentSession) requires `parent` and
/// `provider` and forbids the PTY-shape fields; a
/// [`Terminal`](ResourceKind::Terminal) forbids `parent`, `provider`, and
/// `native_id`; an [`Unknown`](ResourceKind::Unknown) kind is passed through
/// so the server can answer [`SpawnError::UnsupportedKind`]. A client MUST
/// see [`ServerFeature::ResourceKinds`](crate::caps::ServerFeature::ResourceKinds)
/// before asking for another kind.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpawnResource {
    /// Kind to spawn (field 11; absent means `Terminal`).
    pub kind: ResourceKind,
    /// Parent resource (field 12); closing it closes the child.
    pub parent: Option<ResourceId>,
    /// Agent provider name, e.g. `claude` (field 13; at most
    /// [`MAX_RESOURCE_PROVIDER_BYTES`](super::MAX_RESOURCE_PROVIDER_BYTES)
    /// bytes, non-empty). Required for `AgentSession`.
    pub provider: Option<String>,
    /// Opaque provider-native session id (field 14; at most
    /// [`MAX_RESOURCE_NATIVE_ID_BYTES`](super::MAX_RESOURCE_NATIVE_ID_BYTES)
    /// bytes, non-empty).
    pub native_id: Option<String>,
    /// Bind the new resource to the server's instance token (field 15,
    /// ADR-0109), answered with [`SpawnResult::OkBound`] by a server
    /// advertising [`ServerFeature::ConditionalKill`](crate::caps::ServerFeature::ConditionalKill).
    pub bind_instance: bool,
    /// Keep a Terminal inspectable this many seconds after exit (field 16,
    /// ADR-0124); `Some(0)` is the server default. Needs
    /// [`ServerFeature::RetainOnExit`](crate::caps::ServerFeature::RetainOnExit).
    pub retain_secs: Option<u32>,
    /// Idempotency key (field 17, ADR-0126): a same-payload repeat answers
    /// [`SpawnResult::Replayed`]. Needs
    /// [`ServerFeature::SpawnIdempotency`](crate::caps::ServerFeature::SpawnIdempotency).
    pub idempotency_key: Option<IdempotencyKey>,
}

impl SpawnResource {
    /// The fields of an `AgentSession` spawn bound to `parent`: the two the
    /// decoder requires, with `native_id` left to [`Self::with_native_id`].
    #[must_use]
    pub fn agent_session(parent: ResourceId, provider: impl Into<String>) -> Self {
        Self {
            kind: ResourceKind::AgentSession,
            parent: Some(parent),
            provider: Some(provider.into()),
            ..Self::default()
        }
    }

    /// Builder setter for [`Self::retain_secs`].
    #[must_use]
    pub const fn with_retain_secs(mut self, retain_secs: Option<u32>) -> Self {
        self.retain_secs = retain_secs;
        self
    }

    /// Builder setter for [`Self::idempotency_key`].
    #[must_use]
    pub const fn with_idempotency_key(mut self, key: Option<IdempotencyKey>) -> Self {
        self.idempotency_key = key;
        self
    }

    /// Builder setter for [`Self::native_id`].
    #[must_use]
    pub fn with_native_id(mut self, native_id: Option<String>) -> Self {
        self.native_id = native_id;
        self
    }

    /// Builder setter for [`Self::bind_instance`].
    #[must_use]
    pub const fn with_bind_instance(mut self, bind_instance: bool) -> Self {
        self.bind_instance = bind_instance;
        self
    }

    /// `true` iff every field is at its default: a plain Terminal spawn,
    /// which the encoder writes as no fields at all.
    #[must_use]
    pub const fn is_default(&self) -> bool {
        self.kind.is_terminal()
            && self.parent.is_none()
            && self.provider.is_none()
            && self.native_id.is_none()
            && !self.bind_instance
            && self.retain_secs.is_none()
            && self.idempotency_key.is_none()
    }
}

/// Session the client wishes to attach to (SPEC §13).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AttachTarget {
    /// Most-recently-touched live session known to the server. Before any
    /// touch, resolves to the server's configured live seed. Returns
    /// `SESSION_NOT_FOUND` when neither resolution yields a live session;
    /// never creates.
    Last,
    /// Look up a session by its human-readable name.
    ByName(String),
    /// Look up a session by its server-assigned [`SessionId`].
    ById(SessionId),
    /// Look up a session by name; create one if no such session exists.
    CreateIfMissing {
        /// Name for the new session (also used to match an existing one).
        name: String,
        /// Initial command to run in the seed pane, if creation occurs.
        command: Option<Vec<String>>,
        /// Working directory for the seed pane, if creation occurs.
        cwd: Option<String>,
    },
}

/// Viewport metrics the client advertises (SPEC §13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ViewportInfo {
    /// Viewport width in cells.
    pub cols: u16,
    /// Viewport height in cells.
    pub rows: u16,
    /// Optional viewport width in pixels.
    pub pixel_w: Option<u16>,
    /// Optional viewport height in pixels.
    pub pixel_h: Option<u16>,
}

impl ViewportInfo {
    /// A viewport of `cols` x `rows` cells with no pixel size.
    #[must_use]
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            pixel_w: None,
            pixel_h: None,
        }
    }

    /// Builder setter for the optional pixel dimensions.
    #[must_use]
    pub const fn with_pixels(mut self, pixel_w: Option<u16>, pixel_h: Option<u16>) -> Self {
        self.pixel_w = pixel_w;
        self.pixel_h = pixel_h;
        self
    }
}

/// Scope of an L3 metadata key (SPEC §7.4): a 1-byte tag (`0` resource,
/// `1` group, `2` global) plus the id, if any.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Scope {
    /// Keys scoped to a single resource. Cleared when the resource closes.
    Resource(ResourceId),
    /// Keys scoped to a Group (opaque grouping key).
    Group(GroupId),
    /// Server-wide keys.
    Global,
}

/// Why a spawn was refused (SPEC §7.2 / §10.1). Unknown tags decode as
/// [`DecodeError::UnknownEnumValue`](crate::wire::error::DecodeError::UnknownEnumValue).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SpawnError {
    /// The named `group` does not exist.
    GroupNotFound,
    /// Spawning failed; carries a diagnostic.
    SpawnFailed(String),
    /// This server cannot route to the named satellite (not a hub, or the
    /// host is unregistered).
    UnsupportedSatelliteRoute,
    /// The hub's link to the named satellite is down; retryable.
    SatelliteUnreachable(String),
    /// This server does not serve the requested `kind`.
    UnsupportedKind,
    /// The `parent` does not exist.
    ParentNotFound,
    /// The `parent` may not own this child kind.
    ParentKindMismatch,
    /// The `idempotency_key` is bound to a different payload (ADR-0126);
    /// nothing was created.
    IdempotencyConflict,
}

/// The `RESOURCE_SPAWNED` result (SPEC §7.2 / §10.1).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SpawnResult {
    /// The freshly spawned Terminal's identifier.
    Ok(ResourceId),
    /// Structured failure; see [`SpawnError`].
    Err(SpawnError),
    /// The new id plus the instance token, answering
    /// [`SpawnResource::bind_instance`] (ADR-0109): the `Ok` tag plus field 3.
    OkBound {
        /// The freshly spawned resource.
        id: ResourceId,
        /// The token naming the id space `id` was allocated from.
        instance: ServerInstance,
    },
    /// The id an earlier same-key spawn created; nothing new was spawned
    /// (ADR-0126). The `Ok` tag plus field 4 (and field 3 when bound).
    Replayed {
        /// The resource the original spawn created.
        id: ResourceId,
        /// Its instance token, when the original spawn asked to bind it.
        instance: Option<ServerInstance>,
    },
}

impl SpawnResult {
    /// The spawned resource's id, bound, replayed, or neither; `None` for a
    /// refusal.
    #[must_use]
    pub const fn spawned_id(&self) -> Option<&ResourceId> {
        match self {
            Self::Ok(id) | Self::OkBound { id, .. } | Self::Replayed { id, .. } => Some(id),
            Self::Err(_) => None,
        }
    }

    /// The instance token the spawned id is bound to, when the server bound
    /// it.
    #[must_use]
    pub const fn instance(&self) -> Option<ServerInstance> {
        match self {
            Self::OkBound { instance, .. } => Some(*instance),
            Self::Replayed { instance, .. } => *instance,
            Self::Ok(_) | Self::Err(_) => None,
        }
    }

    /// `true` iff the reply repeats an earlier keyed spawn.
    #[must_use]
    pub const fn is_replayed(&self) -> bool {
        matches!(self, Self::Replayed { .. })
    }
}

/// Why a move was refused (ADR-0056).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MoveError {
    /// The move was refused or failed; carries a diagnostic.
    MoveFailed(String),
    /// A satellite-tagged Terminal was named; moves are local-only.
    UnsupportedSatelliteRoute,
}

/// The `RESOURCE_MOVED` result (ADR-0056); the id is stable across a move.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MoveResult {
    /// The moved Terminal's identifier (stable across the move).
    Ok(ResourceId),
    /// Structured failure; see [`MoveError`].
    Err(MoveError),
}
