//! Resource identity: the server instance, satellite hosts, [`ResourceId`],
//! [`ResourceKind`], and [`FrameId`].

/// The server's resource id space, 16 random bytes (`docs/spec/L1.md` §3.1,
/// ADR-0109).
///
/// Re-minted whenever the `ResourceId` allocator restarts, so
/// `KILL_RESOURCE_IF` cannot land on a pane that merely reuses an id. Opaque;
/// distinct from `HELLO_OK.server_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServerInstance([u8; 16]);

impl ServerInstance {
    /// Wrap 16 token bytes.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the 16-byte wire representation.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Opaque federation host token for a [`ResourceId::Satellite`] (ADR-0007).
/// A `Box<str>` keeps [`ResourceId`] at 24 bytes.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct SatelliteHost(Box<str>);

impl SatelliteHost {
    /// Wrap a host token verbatim; the federation handshake validates it.
    #[must_use]
    pub fn new(host: impl Into<String>) -> Self {
        Self(host.into().into_boxed_str())
    }

    /// Borrow the underlying host token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume and return the underlying host token.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0.into_string()
    }
}

impl core::fmt::Display for SatelliteHost {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for SatelliteHost {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for SatelliteHost {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// Wire tag byte for [`ResourceId::Local`].
pub const RESOURCE_ID_TAG_LOCAL: u8 = 0;
/// Wire tag byte for [`ResourceId::Satellite`].
pub const RESOURCE_ID_TAG_SATELLITE: u8 = 1;

/// Wire identifier for a served resource (ADR-0016): owned by this server,
/// or by a federation peer. A non-hub decoder MUST accept `Satellite` and
/// answer [`UnsupportedSatelliteRoute`] (SPEC §14).
///
/// [`UnsupportedSatelliteRoute`]: crate::wire::frame::ErrorCode::UnsupportedSatelliteRoute
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum ResourceId {
    /// A terminal owned by the receiving server (wire tag = 0).
    Local {
        /// Monotonic per-server identifier.
        id: u32,
    },
    /// A terminal owned by a federation peer (wire tag = 1). A hub rewrites
    /// it to the peer's `Local` space and re-tags replies (SPEC L1 §9.1).
    Satellite {
        /// Federation peer that owns the terminal.
        host: SatelliteHost,
        /// Peer-local identifier (scope: `host`).
        id: u32,
    },
}

impl ResourceId {
    /// Construct a `Local` terminal id.
    #[must_use]
    pub const fn local(id: u32) -> Self {
        Self::Local { id }
    }

    /// Construct a `Satellite` terminal id. Non-hub servers MUST NOT emit one.
    #[must_use]
    pub fn satellite(host: impl Into<SatelliteHost>, id: u32) -> Self {
        Self::Satellite {
            host: host.into(),
            id,
        }
    }

    /// Same as [`Self::local`].
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self::local(raw)
    }

    /// `Some(id)` for a [`Local`](Self::Local) id, `None` for a satellite.
    #[must_use]
    pub const fn local_id(&self) -> Option<u32> {
        match self {
            Self::Local { id } => Some(*id),
            Self::Satellite { .. } => None,
        }
    }

    /// The federation host that owns this terminal, or `None` for
    /// [`Local`](Self::Local).
    #[must_use]
    pub const fn host(&self) -> Option<&SatelliteHost> {
        match self {
            Self::Local { .. } => None,
            Self::Satellite { host, .. } => Some(host),
        }
    }

    /// `true` iff this is a [`Local`](Self::Local) terminal id.
    #[must_use]
    pub const fn is_local(&self) -> bool {
        matches!(self, Self::Local { .. })
    }
}

impl Default for ResourceId {
    fn default() -> Self {
        Self::local(0)
    }
}

/// The canonical direct selector a user types back (`@N`, `host/@N`), so an
/// error message that names a resource names it the way the CLI addresses
/// it. `Debug` keeps the structural form.
impl core::fmt::Display for ResourceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Local { id } => write!(f, "@{id}"),
            Self::Satellite { host, id } => write!(f, "{host}/@{id}"),
        }
    }
}

/// Wire tag byte for [`ResourceKind::Terminal`].
pub const RESOURCE_KIND_TAG_TERMINAL: u8 = 0;
/// Wire tag byte for [`ResourceKind::AgentSession`].
pub const RESOURCE_KIND_TAG_AGENT_SESSION: u8 = 1;

/// What backs a served resource.
///
/// An open `u8` enum: an unrecognised tag decodes as [`ResourceKind::Unknown`]
/// so an older peer refuses the operation instead of dropping the connection.
/// Tags are never reused.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[non_exhaustive]
pub enum ResourceKind {
    /// A PTY plus libghostty terminal (wire tag = 0); the only kind that
    /// takes input, resize, screen reads, history, and signals.
    #[default]
    Terminal,
    /// A producer-fed agent event stream bound to a Terminal (wire tag = 1).
    AgentSession,
    /// A kind this build does not recognise, preserved for relays.
    Unknown {
        /// The unrecognised wire tag.
        tag: u8,
    },
}

impl ResourceKind {
    /// Stable wire tag byte.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        match self {
            Self::Terminal => RESOURCE_KIND_TAG_TERMINAL,
            Self::AgentSession => RESOURCE_KIND_TAG_AGENT_SESSION,
            Self::Unknown { tag } => tag,
        }
    }

    /// Decode a wire tag; never fails.
    #[must_use]
    pub const fn from_wire(tag: u8) -> Self {
        match tag {
            RESOURCE_KIND_TAG_TERMINAL => Self::Terminal,
            RESOURCE_KIND_TAG_AGENT_SESSION => Self::AgentSession,
            other => Self::Unknown { tag: other },
        }
    }

    /// `true` iff this is the PTY-backed Terminal kind.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Terminal)
    }

    /// Lower-case stable name (`terminal`, `agent_session`, `unknown`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::AgentSession => "agent_session",
            Self::Unknown { .. } => "unknown",
        }
    }
}

impl core::fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unknown { tag } => write!(f, "unknown({tag})"),
            other => f.write_str(other.as_str()),
        }
    }
}

/// Monotonic per-terminal frame id; `0` is the empty initial frame.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct FrameId(pub u64);

impl FrameId {
    /// The empty initial frame, before any output.
    pub const ZERO: Self = Self(0);

    /// Advance to the next frame.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

impl core::fmt::Display for FrameId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "FrameId({})", self.0)
    }
}
