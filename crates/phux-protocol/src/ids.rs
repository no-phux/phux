//! Stable identifiers used across the protocol.
//!
//! Most IDs are opaque `u32` values, monotonically allocated by the server.
//! IDs are stable for the server's lifetime and are not reused after the
//! entity is destroyed.
//!
//! [`TerminalId`] is the exception: per [ADR-0016] it is a tagged union that
//! also records the host that owns the terminal. Non-hub servers only ever
//! construct [`TerminalId::Local`]; the [`TerminalId::Satellite`] variant is
//! how a federation hub addresses (and re-tags) satellite-owned terminals
//! per [ADR-0007].
//!
//! [ADR-0007]: https://github.com/phall1/phux/blob/main/ADR/0007-mosh-class-transport-and-satellites.md
//! [ADR-0016]: https://github.com/phall1/phux/blob/main/ADR/0016-terminal-id-as-wire-primary.md

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
    /// Opaque grouping key, formerly the L2 "Collection" lifecycle tier.
    ///
    /// The "Option B" re-tier (v0.3.0, ADR-0019 / ADR-0027) **dissolved the
    /// L2 collection tier**: there is no collection lifecycle anymore.
    /// Grouping (membership + names) is now L3 metadata plus client logic,
    /// and the lifecycle verbs that needed a collection id
    /// (`CREATE_SESSION` / `KILL_COLLECTION` / `RENAME_SESSION`) were
    /// removed. `GroupId` survives only as a documented **opaque
    /// grouping key** because it is still threaded through three surviving
    /// surfaces that would balloon the re-tier if removed in the same pass:
    /// the `Scope::Group` L3-metadata scope (`docs/spec/L3.md` §1),
    /// the `SpawnTerminal.group` field, and the `CommandValue::GroupId`
    /// reply variant. Removing it entirely is a follow-up bead.
    ///
    /// It is **not** a lifecycle tier: v0.3 servers expose a single static
    /// default `GroupId(1)` and treat it as an opaque scope label, not
    /// a thing with create/kill/rename semantics. The wire encoding is the
    /// inner `u32`.
    GroupId
);

/// Opaque client-generated identifier for one acknowledged input operation.
///
/// The all-zero value is reserved and cannot be constructed. Debug output is
/// deliberately redacted because operation identifiers may be correlated with
/// sensitive input activity.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct InputOperationId([u8; 16]);

impl InputOperationId {
    /// Construct a non-zero operation identifier.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Option<Self> {
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != 0 {
                return Some(Self(bytes));
            }
            index += 1;
        }
        None
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

/// Opaque client-generated identifier for one chunked file upload.
///
/// The all-zero value is reserved and cannot be constructed. The identifier
/// names the server-side partial file across reconnects, so retrying a chunk
/// with the same id and offset is idempotent. Debug output is redacted because
/// identifiers can be correlated with user files.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileUploadId([u8; 16]);

impl FileUploadId {
    /// Construct a non-zero upload identifier.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Option<Self> {
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != 0 {
                return Some(Self(bytes));
            }
            index += 1;
        }
        None
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

/// Federation-routing host identifier for a [`TerminalId::Satellite`].
///
/// Per [ADR-0007] the satellite link is an opaque host token negotiated at
/// federation-handshake time. v0 keeps the shape minimal: a length-prefixed
/// UTF-8 string. Concrete host syntax (hostnames, ULIDs, mosh-keys) is the
/// federation layer's concern; the wire treats it as bytes.
///
/// [ADR-0007]: https://github.com/phall1/phux/blob/main/ADR/0007-mosh-class-transport-and-satellites.md
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct SatelliteHost(String);

impl SatelliteHost {
    /// Wrap a host token. The string is taken verbatim; no validation is
    /// performed here — the federation handshake validates upstream.
    #[must_use]
    pub fn new(host: impl Into<String>) -> Self {
        Self(host.into())
    }

    /// Borrow the underlying host token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume and return the underlying host token.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl core::fmt::Display for SatelliteHost {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for SatelliteHost {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SatelliteHost {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// Wire tag byte for [`TerminalId::Local`].
pub const TERMINAL_ID_TAG_LOCAL: u8 = 0;
/// Wire tag byte for [`TerminalId::Satellite`].
pub const TERMINAL_ID_TAG_SATELLITE: u8 = 1;

/// Wire identifier for a managed terminal, per [ADR-0016].
///
/// `TerminalId` is a tagged union: [`Local`](Self::Local) names a terminal
/// owned by this server; [`Satellite`](Self::Satellite) names a terminal
/// reachable through a federation peer. v0.1 servers only ever construct
/// `Local`; v0.1 decoders MUST accept the `Satellite` tag and respond with
/// [`UnsupportedSatelliteRoute`] (per SPEC §14) if not configured as a
/// federation hub.
///
/// The numeric `id` inside each variant is stable for the life of the
/// owning server and is not reused after the terminal closes.
///
/// [ADR-0016]: https://github.com/phall1/phux/blob/main/ADR/0016-terminal-id-as-wire-primary.md
/// [`UnsupportedSatelliteRoute`]: crate::wire::frame::ErrorCode::UnsupportedSatelliteRoute
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum TerminalId {
    /// A terminal owned by the receiving server (wire tag = 0).
    Local {
        /// Monotonic per-server identifier.
        id: u32,
    },
    /// A terminal owned by a federation peer (wire tag = 1).
    ///
    /// A federation hub (ADR-0007) relays frames carrying this tag over
    /// its outbound satellite links, rewriting to the peer's `Local`
    /// space outbound and re-tagging responses/streams on the way back
    /// (SPEC L1 §9.1). Decoders on non-hub servers MUST still accept the
    /// shape and respond with [`UnsupportedSatelliteRoute`]; a hub whose
    /// link to `host` is down responds with [`SatelliteUnreachable`].
    ///
    /// [`UnsupportedSatelliteRoute`]: crate::wire::frame::ErrorCode::UnsupportedSatelliteRoute
    /// [`SatelliteUnreachable`]: crate::wire::frame::ErrorCode::SatelliteUnreachable
    Satellite {
        /// Federation peer that owns the terminal.
        host: SatelliteHost,
        /// Peer-local identifier (scope: `host`).
        id: u32,
    },
}

impl TerminalId {
    /// Construct a `Local` terminal id from a raw `u32`.
    ///
    /// This is the v0.1 hot path — every terminal allocated by a v0.1
    /// server flows through this constructor.
    #[must_use]
    pub const fn local(id: u32) -> Self {
        Self::Local { id }
    }

    /// Construct a `Satellite` terminal id.
    ///
    /// Constructed by federation hubs when re-tagging satellite-owned
    /// terminals for their consumers (ADR-0007), and by consumers
    /// addressing those terminals. Non-hub servers MUST NOT emit
    /// `Satellite` ids.
    #[must_use]
    pub fn satellite(host: impl Into<SatelliteHost>, id: u32) -> Self {
        Self::Satellite {
            host: host.into(),
            id,
        }
    }

    /// Construct from a raw `u32`, defaulting to the `Local` variant.
    ///
    /// Compatibility shim for call sites that historically held a bare
    /// `u32` from the wire — equivalent to `TerminalId::local(raw)`.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self::local(raw)
    }

    /// Returns `Some(id)` for [`Local`](Self::Local) terminals, `None` for
    /// [`Satellite`](Self::Satellite).
    ///
    /// Use this at boundaries that have no satellite story yet (server
    /// dispatch tables keyed by `u32`, logging, etc.). A `None` is a
    /// signal to respond with [`UnsupportedSatelliteRoute`] or drop the
    /// frame with a warn, per SPEC §10.1.
    ///
    /// [`UnsupportedSatelliteRoute`]: crate::wire::frame::ErrorCode::UnsupportedSatelliteRoute
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

impl Default for TerminalId {
    fn default() -> Self {
        Self::local(0)
    }
}

impl core::fmt::Display for TerminalId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Local { id } => write!(f, "TerminalId({id})"),
            Self::Satellite { host, id } => write!(f, "TerminalId({host}/{id})"),
        }
    }
}

/// Identifier for a terminal frame. Monotonically increasing per terminal; `0`
/// is the empty initial frame.
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
    use super::InputOperationId;

    #[test]
    fn input_operation_id_rejects_zero_and_redacts_debug() {
        assert!(InputOperationId::new([0; 16]).is_none());
        let id = InputOperationId::new([0x5a; 16]).expect("non-zero id");
        assert_eq!(id.as_bytes(), &[0x5a; 16]);
        assert_eq!(format!("{id:?}"), "InputOperationId(<redacted>)");
        assert!(!format!("{id:?}").contains("5a"));
    }
}
