//! Stable identifiers used across the protocol.
//!
//! Most IDs are opaque server-allocated `u32`s, never reused within a server's
//! lifetime. [`ResourceId`] is a tagged union that also names the owning
//! federation host (ADR-0016, ADR-0007).

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
        pub struct $name(pub u32);

        impl $name {
            /// Construct from a raw `u32`.
            #[must_use]
            pub const fn new(raw: u32) -> Self {
                Self(raw)
            }

            /// Inner raw value.
            #[must_use]
            pub const fn get(self) -> u32 {
                self.0
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }
    };
}

id_type!(
    /// Identifier for a session within a server.
    SessionId
);
id_type!(
    /// Identifier for a window within a server.
    WindowId
);
id_type!(
    /// Identifier for a currently-connected client.
    ClientId
);
id_type!(
    /// Opaque grouping key: the `Scope::Group` metadata scope, the spawn
    /// `group` field, and `CommandValue::GroupId`. Not a lifecycle tier
    /// (ADR-0019, ADR-0027); servers expose a single static `GroupId(1)`.
    GroupId
);
/// Non-zero, connection-scoped identifier for one logical terminal
/// subscription, allocated by the originating endpoint. Relays map it rather
/// than reuse it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct StreamId(core::num::NonZeroU64);

impl StreamId {
    /// Construct a stream identifier, returning `None` for the reserved zero value.
    #[must_use]
    pub const fn new(raw: u64) -> Option<Self> {
        match core::num::NonZeroU64::new(raw) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Return the non-zero wire value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl core::fmt::Display for StreamId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "StreamId({})", self.get())
    }
}

/// Non-zero identifier for one terminal replica generation. Every bootstrap
/// gets a new one; no frame may carry a tombstoned generation's id.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct BootstrapId(core::num::NonZeroU64);

impl BootstrapId {
    /// Construct a bootstrap identifier, returning `None` for the reserved zero value.
    #[must_use]
    pub const fn new(raw: u64) -> Option<Self> {
        match core::num::NonZeroU64::new(raw) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Return the non-zero wire value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl core::fmt::Display for BootstrapId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "BootstrapId({})", self.get())
    }
}

/// Whether a 16-byte id is not the reserved all-zero value.
const fn is_nonzero(bytes: &[u8; 16]) -> bool {
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0 {
            return true;
        }
        index += 1;
    }
    false
}

/// Opaque, non-zero, client-generated id for one acknowledged input
/// operation. Debug is redacted: ids correlate with sensitive input.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct InputOperationId([u8; 16]);

impl InputOperationId {
    /// Construct a non-zero operation identifier.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Option<Self> {
        if is_nonzero(&bytes) {
            Some(Self(bytes))
        } else {
            None
        }
    }

    /// Borrow the 16-byte wire representation.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl core::fmt::Debug for InputOperationId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("InputOperationId(<redacted>)")
    }
}

/// Opaque, non-zero, CSPRNG-drawn key that makes one create idempotent
/// (`SPAWN_RESOURCE` field 17, ADR-0126; `EVENT.operation_id`). Debug is
/// redacted.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct IdempotencyKey([u8; 16]);

impl IdempotencyKey {
    /// Construct a non-zero key.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Option<Self> {
        if is_nonzero(&bytes) {
            Some(Self(bytes))
        } else {
            None
        }
    }

    /// Borrow the 16-byte wire representation.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl core::fmt::Debug for IdempotencyKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("IdempotencyKey(<redacted>)")
    }
}

/// Opaque, non-zero, client-generated id for one chunked upload. It names
/// the partial file across reconnects, so a retried chunk is idempotent.
/// Debug is redacted.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileUploadId([u8; 16]);

impl FileUploadId {
    /// Construct a non-zero upload identifier.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Option<Self> {
        if is_nonzero(&bytes) {
            Some(Self(bytes))
        } else {
            None
        }
    }

    /// Borrow the 16-byte wire representation.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl core::fmt::Debug for FileUploadId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("FileUploadId(<redacted>)")
    }
}

/// Server-minted, non-zero id of one held action awaiting approval
/// (ADR-0128); text form is 32 lowercase hex digits. It names a request, not
/// an authority: a decision is authorized by the decider's grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ApprovalId([u8; 16]);

impl ApprovalId {
    /// Construct a non-zero approval id.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Option<Self> {
        if is_nonzero(&bytes) {
            Some(Self(bytes))
        } else {
            None
        }
    }

    /// Borrow the 16-byte wire representation.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// The approval a canonical `phux.approval.decide/v1/<id>` key names.
    #[must_use]
    pub fn from_decide_key(key: &str) -> Option<Self> {
        key.strip_prefix(crate::wire::frame::APPROVAL_DECIDE_KEY_PREFIX)
            .and_then(Self::parse)
    }

    /// The server-owned record key, `phux.approval/v1/<id>`.
    #[must_use]
    pub fn record_key(&self) -> String {
        format!("{}{self}", crate::wire::frame::APPROVAL_KEY_PREFIX)
    }

    /// The decision key, `phux.approval.decide/v1/<id>`.
    #[must_use]
    pub fn decide_key(&self) -> String {
        format!("{}{self}", crate::wire::frame::APPROVAL_DECIDE_KEY_PREFIX)
    }

    /// Parse the text form: exactly 32 lowercase hex digits, not all zero.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.as_bytes();
        if digits.len() != 32 {
            return None;
        }
        let mut bytes = [0_u8; 16];
        for (byte, pair) in bytes.iter_mut().zip(digits.as_chunks::<2>().0) {
            *byte = (lower_hex_value(pair[0])? << 4) | lower_hex_value(pair[1])?;
        }
        Self::new(bytes)
    }
}

/// The value of one lowercase hex digit.
const fn lower_hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

impl core::fmt::Display for ApprovalId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Lowercase RFC 4648 base32 alphabet, the text form of a 16-byte key.
const BASE32_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Characters in the text form of a 16-byte key: `ceil(128 / 5)`.
pub const KEY_TEXT_LEN: usize = 26;

/// Shortest prefix a display may shorten a key's text form to; a parser
/// always accepts the full form.
pub const KEY_MIN_PREFIX: usize = 6;

/// Write `bytes` as unpadded lowercase base32.
fn write_base32(bytes: &[u8; 16], f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    use core::fmt::Write as _;
    let mut buffer: u32 = 0;
    let mut bits = 0_u32;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            f.write_char(char::from(
                BASE32_ALPHABET[((buffer >> bits) & 0x1f) as usize],
            ))?;
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        f.write_char(char::from(
            BASE32_ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize],
        ))?;
    }
    Ok(())
}

/// The value of one base32 digit, either case.
const fn base32_value(digit: u8) -> Option<u32> {
    match digit {
        b'a'..=b'z' => Some((digit - b'a') as u32),
        b'A'..=b'Z' => Some((digit - b'A') as u32),
        b'2'..=b'7' => Some((digit - b'2') as u32 + 26),
        _ => None,
    }
}

/// Parse exactly [`KEY_TEXT_LEN`] base32 digits into 16 bytes. The final
/// digit's two padding bits must be zero, so every key has one spelling.
fn parse_base32(text: &str) -> Option<[u8; 16]> {
    let digits = text.as_bytes();
    if digits.len() != KEY_TEXT_LEN {
        return None;
    }
    let mut out = [0_u8; 16];
    let mut buffer: u32 = 0;
    let mut bits = 0_u32;
    let mut index = 0;
    for digit in digits {
        buffer = (buffer << 5) | base32_value(*digit)?;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            *out.get_mut(index)? = u8::try_from(buffer >> bits).ok()?;
            index += 1;
        }
        buffer &= (1 << bits) - 1;
    }
    (index == 16 && buffer == 0).then_some(out)
}

/// A key's text form was not [`KEY_TEXT_LEN`] base32 digits naming a
/// non-zero key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a key is 26 base32 digits naming a non-zero 16-byte value")]
pub struct KeyParseError;

/// Declare a persistent 16-byte organization key (`docs/spec/L1.md` §5.7):
/// random, never all-zero, 16 raw bytes on the wire, unpadded lowercase
/// base32 as text. Keys name records forever; live handles (`SessionId`,
/// `ResourceId`) name current execution and never stand in for one.
macro_rules! key16 {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; 16]);

        impl $name {
            /// Wrap 16 key bytes; `None` for the reserved all-zero value.
            #[must_use]
            pub const fn new(bytes: [u8; 16]) -> Option<Self> {
                if is_nonzero(&bytes) {
                    Some(Self(bytes))
                } else {
                    None
                }
            }

            /// Borrow the 16-byte wire representation.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 16] {
                &self.0
            }

            /// Parse the full text form: [`KEY_TEXT_LEN`] base32 digits,
            /// either case, not all zero.
            #[must_use]
            pub fn parse(text: &str) -> Option<Self> {
                parse_base32(text).and_then(Self::new)
            }

            /// Whether the text form starts with `prefix`, ignoring case: how
            /// a shortened key a user typed back is resolved.
            #[must_use]
            pub fn has_prefix(&self, prefix: &str) -> bool {
                let text = self.to_string();
                text.len() >= prefix.len()
                    && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write_base32(&self.0, f)
            }
        }

        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}({self})", stringify!($name))
            }
        }

        impl core::str::FromStr for $name {
            type Err = KeyParseError;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                Self::parse(text).ok_or(KeyParseError)
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
                Self::parse(&text).ok_or_else(|| {
                    serde::de::Error::custom(concat!("invalid ", stringify!($name)))
                })
            }
        }
    };
}

key16!(
    /// The organization authority: minted once when the org store is first
    /// created and never changed. Refs across machines prefix it.
    AuthorityId
);
key16!(
    /// A session record's persistent key, unique per authority.
    SessionKey
);
key16!(
    /// A resource record's persistent key, unique per authority.
    ResourceKey
);
key16!(
    /// A workspace record's persistent key, unique per authority.
    WorkspaceKey
);
key16!(
    /// An arrangement record's persistent key, unique per authority.
    ArrangementKey
);
key16!(
    /// An acting principal: the owner human, or an owner-registered
    /// credential.
    PrincipalId
);

/// The caller-generated id of one organization operation.
///
/// The same type as the keyed-supervisory [`IdempotencyKey`]: 16 non-zero
/// bytes with a redacted Debug, in one retry space, so an organization
/// result and an `EVENT.operation_id` stamp correlate without conversion.
pub type OperationId = IdempotencyKey;

/// Declare a persisted `u64` counter newtype.
macro_rules! counter_u64 {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default,
            serde::Serialize, serde::Deserialize,
        )]
        pub struct $name(pub u64);

        impl $name {
            /// Construct from a raw `u64`.
            #[must_use]
            pub const fn new(raw: u64) -> Self {
                Self(raw)
            }

            /// Inner raw value.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

counter_u64!(
    /// One execution of a resource: monotonic from 1 per resource, persisted.
    ActivationId
);
counter_u64!(
    /// A resource's input-lease epoch; bumps on every lease change.
    LeaseEpoch
);
counter_u64!(
    /// A record's revision; bumps on every committed change to that record.
    Revision
);

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

#[cfg(test)]
mod tests {
    use super::{InputOperationId, ResourceKind};

    #[test]
    fn resource_kind_tags_are_stable_and_unknown_round_trips() {
        assert_eq!(ResourceKind::Terminal.as_wire(), 0);
        assert_eq!(ResourceKind::AgentSession.as_wire(), 1);
        assert_eq!(ResourceKind::from_wire(0), ResourceKind::Terminal);
        assert_eq!(ResourceKind::from_wire(1), ResourceKind::AgentSession);
        assert_eq!(
            ResourceKind::from_wire(200),
            ResourceKind::Unknown { tag: 200 }
        );
        assert_eq!(ResourceKind::Unknown { tag: 200 }.as_wire(), 200);
        assert_eq!(ResourceKind::default(), ResourceKind::Terminal);
        assert!(ResourceKind::Terminal.is_terminal());
        assert!(!ResourceKind::AgentSession.is_terminal());
        assert_eq!(ResourceKind::AgentSession.to_string(), "agent_session");
        assert_eq!(ResourceKind::Unknown { tag: 9 }.to_string(), "unknown(9)");
    }

    #[test]
    fn input_operation_id_rejects_zero_and_redacts_debug() {
        assert!(InputOperationId::new([0; 16]).is_none());
        let id = InputOperationId::new([0x5a; 16]).expect("non-zero id");
        assert_eq!(id.as_bytes(), &[0x5a; 16]);
        assert_eq!(format!("{id:?}"), "InputOperationId(<redacted>)");
        assert!(!format!("{id:?}").contains("5a"));
    }
}
