//! Structured error codes and teardown/rejection reason enums (SPEC §14).

/// Structured error code carried by [`FrameKind::Error`](super::FrameKind::Error)
/// (SPEC §14), a big-endian `u16`.
///
/// Unknown values decode as
/// [`DecodeError::UnknownEnumValue`](crate::wire::error::DecodeError::UnknownEnumValue),
/// never a placeholder. Ranges: handshake `1..=9`, attach `100..=199`,
/// command `200..=299`, internal `u16::MAX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(u16)]
pub enum ErrorCode {
    /// SPEC §6.1: HELLO version negotiation found no compatible version.
    VersionIncompatible = 1,
    /// SPEC §6: the peer sent a type byte the receiver does not recognise.
    UnknownMessageType = 2,
    /// SPEC §5 / Appendix A: a message could not be decoded.
    MalformedMessage = 3,
    /// SPEC §5: a frame's declared length exceeded the protocol cap.
    FrameTooLarge = 4,
    // Value 5 is permanently reserved for the withdrawn OUT_OF_TIER error.
    /// SPEC §6.1: peers share no usable explicit bootstrap profile/codec/features.
    CodecUnavailable = 6,

    /// SPEC §13: the client issued an operation that requires an attach
    /// while not attached.
    NotAttached = 100,
    /// SPEC §13: the client requested attach while already attached.
    AlreadyAttached = 101,
    /// SPEC §13: the requested session does not exist.
    SessionNotFound = 102,
    /// The requested window does not exist.
    WindowNotFound = 103,
    /// The requested terminal does not exist.
    TerminalNotFound = 104,
    /// The requested client id does not exist.
    ClientNotFound = 105,
    /// A `Satellite` id reached a non-hub server, or a hub that does not
    /// know the host (ADR-0016).
    UnsupportedSatelliteRoute = 106,
    /// The hub knows the satellite but its link is down; retryable
    /// (ADR-0007).
    SatelliteUnreachable = 107,

    /// SPEC §11: the requested COMMAND payload was structurally invalid.
    InvalidCommand = 200,
    /// SPEC §15: the requested operation is forbidden for this peer.
    PermissionDenied = 201,
    /// The server has run out of a resource needed to satisfy the request
    /// (file descriptors, memory, PTYs, ...).
    ResourceExhausted = 202,
    /// An untrusted paste in an atomic input batch failed the safety policy.
    UnsafePaste = 203,
    /// A cooperative `ACQUIRE_INPUT` lost to the current lease holder
    /// (ADR-0033).
    InputLeaseHeld = 204,
    /// Input reached the pane write path, but final PTY delivery is unknown.
    InputDeliveryUnknown = 205,
    /// A canonical-mode (`ICANON`) line would exceed the kernel's limit and
    /// be truncated; nothing was written.
    CanonicalLimitExceeded = 206,
    /// `APPLY_INPUT` provably never reached a live PTY writer, so a resubmit
    /// cannot type it twice.
    InputNotWritten = 207,
    /// The verb does not apply to the subject's resource kind (a Terminal
    /// verb on an `AgentSession`, or an append to a Terminal).
    WrongResourceKind = 208,
    /// `APPEND_RESOURCE_OUTPUT` from a client that is not the producer.
    NotProducer = 209,
    /// `APPEND_RESOURCE_OUTPUT` bytes are not complete, valid codec records;
    /// nothing was appended.
    RecordInvalid = 210,
    /// The append's retained ring or rate budget is exhausted; retry after
    /// backoff.
    Overflow = 211,
    /// A `KILL_RESOURCE_IF` precondition did not hold (ADR-0109); nothing
    /// was killed.
    PreconditionFailed = 212,
    /// A hub refused a keyed retry because the owning satellite restarted,
    /// so a replay could run twice (`docs/spec/L1.md` §9.1).
    IncarnationChanged = 213,

    /// Catch-all for unexpected server-side failures.
    InternalError = 65535,
}

/// How far a consumer should degrade on an [`ErrorCode`]. Not a wire field,
/// and not fatality: termination is `DETACHED` plus close (SPEC §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorScope {
    /// One Terminal's work failed. Everything else the consumer holds —
    /// the other panes, the layout, the attach — remains valid.
    Terminal,
    /// One request failed. The consumer's request table owns the outcome;
    /// no projected state changes.
    Request,
    /// The connection is unusable: expect the server's `DETACHED` and close,
    /// and keep reading until they arrive.
    Connection,
}

impl ErrorCode {
    /// Wire encoding of this code: the `#[repr(u16)]` discriminant.
    #[must_use]
    pub const fn as_wire(self) -> u16 {
        self as u16
    }

    /// How far a consumer should degrade on this code. Exhaustive, so a new
    /// code must be classified.
    #[must_use]
    pub const fn scope(self) -> ErrorScope {
        match self {
            Self::VersionIncompatible
            | Self::FrameTooLarge
            | Self::InvalidCommand
            | Self::PermissionDenied => ErrorScope::Connection,
            Self::NotAttached
            | Self::AlreadyAttached
            | Self::SessionNotFound
            | Self::WindowNotFound
            | Self::ClientNotFound
            | Self::UnsafePaste
            | Self::InputLeaseHeld
            | Self::InputDeliveryUnknown
            | Self::CanonicalLimitExceeded
            | Self::InputNotWritten
            | Self::NotProducer
            | Self::RecordInvalid
            | Self::Overflow
            | Self::PreconditionFailed
            | Self::IncarnationChanged => ErrorScope::Request,
            Self::TerminalNotFound
            | Self::WrongResourceKind
            | Self::UnsupportedSatelliteRoute
            | Self::SatelliteUnreachable
            | Self::ResourceExhausted
            | Self::CodecUnavailable
            | Self::MalformedMessage
            | Self::UnknownMessageType
            | Self::InternalError => ErrorScope::Terminal,
        }
    }

    /// Inverse of [`Self::as_wire`]; returns `None` for values that do not
    /// correspond to any code in this protocol version.
    #[must_use]
    pub const fn from_wire(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::VersionIncompatible,
            2 => Self::UnknownMessageType,
            3 => Self::MalformedMessage,
            4 => Self::FrameTooLarge,
            6 => Self::CodecUnavailable,
            100 => Self::NotAttached,
            101 => Self::AlreadyAttached,
            102 => Self::SessionNotFound,
            103 => Self::WindowNotFound,
            104 => Self::TerminalNotFound,
            105 => Self::ClientNotFound,
            106 => Self::UnsupportedSatelliteRoute,
            107 => Self::SatelliteUnreachable,
            200 => Self::InvalidCommand,
            201 => Self::PermissionDenied,
            202 => Self::ResourceExhausted,
            203 => Self::UnsafePaste,
            204 => Self::InputLeaseHeld,
            205 => Self::InputDeliveryUnknown,
            206 => Self::CanonicalLimitExceeded,
            207 => Self::InputNotWritten,
            208 => Self::WrongResourceKind,
            209 => Self::NotProducer,
            210 => Self::RecordInvalid,
            211 => Self::Overflow,
            212 => Self::PreconditionFailed,
            213 => Self::IncarnationChanged,
            65535 => Self::InternalError,
            _ => return None,
        })
    }
}
wire_enum! { as_wire / from_wire;
/// Why a bootstrap generation can no longer preserve stream continuity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TombstoneReason {
    /// The bounded post-cut raw replay queue overflowed.
    RawReplayOverflow = 0,
    /// A live sequence was dropped, duplicated, or observed out of order.
    OutboundGap = 1,
    /// Authoritative PTY geometry changed and requires a new actor cut.
    Resize = 2,
    /// A federation return leg reconnected without provable continuity.
    RelayReconnect = 3,
    /// The consumer explicitly requested a replacement bootstrap.
    ExplicitReattach = 4,
    /// The selected engine/compatibility codec rejected or failed capture.
    CodecFailure = 5,
    /// A bounded, explicit reason not represented by an earlier tag.
    Other = 6,
}
}

wire_enum! { as_wire / from_wire;
/// Why the server ended an attach (`docs/spec/proto.md` §7.2), carried by
/// `DETACHED`.
///
/// An unrecognised byte decodes as `None` (unstated), never as a frame
/// error, so a new reason is never a fleet-wide break (ADR-0061).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DetachReason {
    /// The consumer asked: its own `DETACH`, or an operator's
    /// `DETACH_CLIENTS` sweep on its behalf.
    Requested = 0,
    /// The server process is stopping (`SHUTDOWN`, signal, or supervisor).
    ServerShutdown = 1,
    /// The group the attach was rooted in was torn down (ADR-0030).
    SessionKilled = 2,
    /// Another consumer took over an exclusive attach.
    Replaced = 3,
    /// The peer violated the protocol; the sender is closing the transport.
    /// A fatal `ERROR` MUST be followed by `DETACHED` carrying this reason.
    ProtocolError = 4,
    /// A post-HELLO authentication outcome failed (`workload-auth.md` §7).
    /// A pre-HELLO TLS refusal carries no frame, so it never states this.
    AuthenticationFailed = 5,
    /// The credential behind the connection's authority was revoked, or its
    /// ceiling no longer contains the minted grant (`workload-auth.md` §7).
    AuthorizationRevoked = 6,
    /// The credential behind the connection's authority reached its expiry
    /// (`workload-auth.md` §7).
    AuthorizationExpired = 7,
    /// The server hit an unrecoverable internal fault.
    InternalError = 255,
}
}

impl DetachReason {
    /// One-line human-readable summary, for consumers that surface the
    /// ending on a cooked terminal.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Requested => "detach was requested",
            Self::ServerShutdown => "the server is shutting down",
            Self::SessionKilled => "the session was killed",
            Self::Replaced => "another client took over this attach",
            Self::ProtocolError => "the connection violated the protocol",
            Self::AuthenticationFailed => "authentication failed",
            Self::AuthorizationRevoked => "this connection's authorization was revoked",
            Self::AuthorizationExpired => "this connection's authorization expired",
            Self::InternalError => "the server hit an internal error",
        }
    }
}

/// Why a resource ceased to exist (`RESOURCE_CLOSED.reason`,
/// `docs/spec/L1.md` §3.1).
///
/// An absent field or unrecognised value is [`Unknown`](Self::Unknown),
/// never a decode error, so the ending is never hidden.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum CloseReason {
    /// The process exited on its own, or the producer closed the resource.
    Exited = 0,
    /// A `KILL_RESOURCE` / `KILL_RESOURCES` command removed the resource.
    Killed = 1,
    /// The resource's parent closed.
    ParentClosed = 2,
    /// The server is stopping.
    ServerShutdown = 3,
    /// No reason, or one this build does not know. Encoded as an absent field.
    #[default]
    Unknown = 255,
}

impl CloseReason {
    /// Stable wire discriminant; an encoder omits `Unknown` instead.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        self as u8
    }

    /// Decode a wire value; an unrecognised one is [`Self::Unknown`].
    #[must_use]
    pub const fn from_wire(value: u8) -> Self {
        match value {
            0 => Self::Exited,
            1 => Self::Killed,
            2 => Self::ParentClosed,
            3 => Self::ServerShutdown,
            _ => Self::Unknown,
        }
    }

    /// `true` iff the reason is unstated.
    #[must_use]
    pub const fn is_unknown(self) -> bool {
        matches!(self, Self::Unknown)
    }

    /// One-line human-readable summary.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Exited => "the process exited",
            Self::Killed => "the resource was killed",
            Self::ParentClosed => "the parent resource closed",
            Self::ServerShutdown => "the server is shutting down",
            Self::Unknown => "no reason was stated",
        }
    }
}

wire_enum! { as_wire / from_wire;
/// Why one progressive history cursor can no longer be consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HistoryTombstoneReason {
    /// The cursor is no longer current for its history lease.
    Stale = 0,
    /// The referenced retained rows were pruned.
    Pruned = 1,
    /// History capture state was reset without invalidating live state.
    Reset = 2,
    /// A resize invalidated historical reflow for this cursor.
    Resize = 3,
    /// The cursor lease expired.
    Expired = 4,
    /// The cursor lease was explicitly released.
    Released = 5,
    /// A history byte or row resource limit was reached.
    Limit = 6,
    /// The selected native codec rejected history capture or import.
    CodecFailure = 7,
}
}

wire_enum! { as_wire / from_wire;
/// Why one history request was rejected without advancing its cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HistoryRejectionReason {
    /// A required byte or row request limit was zero.
    ZeroLimit = 0,
    /// The requested limits cannot fit the next independently decodable unit.
    TooSmall = 1,
    /// Capture is temporarily busy; retrying the same cursor is permitted.
    Busy = 2,
}
}

#[cfg(test)]
mod tests {
    use super::{CloseReason, ErrorCode, ErrorScope};

    #[test]
    fn close_reason_tags_are_stable_and_unknown_is_forgiving() {
        for (reason, tag) in [
            (CloseReason::Exited, 0),
            (CloseReason::Killed, 1),
            (CloseReason::ParentClosed, 2),
            (CloseReason::ServerShutdown, 3),
        ] {
            assert_eq!(reason.as_wire(), tag);
            assert_eq!(CloseReason::from_wire(tag), reason);
            assert!(!reason.is_unknown());
        }
        assert_eq!(CloseReason::from_wire(4), CloseReason::Unknown);
        assert_eq!(CloseReason::from_wire(255), CloseReason::Unknown);
        assert!(CloseReason::default().is_unknown());
    }

    /// Every code this protocol version defines, in wire order.
    ///
    /// Hand-maintained on purpose. [`ErrorCode::scope`] already fails to
    /// compile when a variant is added; this list makes the same omission
    /// fail for [`ErrorCode::from_wire`], whose table is equally
    /// hand-written and has no compiler check of its own.
    const ALL_CODES: &[ErrorCode] = &[
        ErrorCode::VersionIncompatible,
        ErrorCode::UnknownMessageType,
        ErrorCode::MalformedMessage,
        ErrorCode::FrameTooLarge,
        ErrorCode::CodecUnavailable,
        ErrorCode::NotAttached,
        ErrorCode::AlreadyAttached,
        ErrorCode::SessionNotFound,
        ErrorCode::WindowNotFound,
        ErrorCode::TerminalNotFound,
        ErrorCode::ClientNotFound,
        ErrorCode::UnsupportedSatelliteRoute,
        ErrorCode::SatelliteUnreachable,
        ErrorCode::InvalidCommand,
        ErrorCode::PermissionDenied,
        ErrorCode::ResourceExhausted,
        ErrorCode::UnsafePaste,
        ErrorCode::InputLeaseHeld,
        ErrorCode::InputDeliveryUnknown,
        ErrorCode::CanonicalLimitExceeded,
        ErrorCode::InputNotWritten,
        ErrorCode::WrongResourceKind,
        ErrorCode::NotProducer,
        ErrorCode::RecordInvalid,
        ErrorCode::Overflow,
        ErrorCode::PreconditionFailed,
        ErrorCode::IncarnationChanged,
        ErrorCode::InternalError,
    ];

    #[test]
    fn the_decodable_wire_space_is_exactly_the_known_codes() {
        for &code in ALL_CODES {
            assert_eq!(ErrorCode::from_wire(code.as_wire()), Some(code), "{code:?}");
        }
        let decoded: Vec<ErrorCode> = (0..=u16::MAX).filter_map(ErrorCode::from_wire).collect();
        assert_eq!(
            decoded, ALL_CODES,
            "a code was added to the enum without a `from_wire` row (or vice versa)"
        );
    }

    #[test]
    fn scopes_partition_the_codes_as_documented() {
        assert_eq!(
            ErrorCode::VersionIncompatible.scope(),
            ErrorScope::Connection
        );
        assert_eq!(ErrorCode::PermissionDenied.scope(), ErrorScope::Connection);
        assert_eq!(ErrorCode::NotAttached.scope(), ErrorScope::Request);
        assert_eq!(
            ErrorCode::CanonicalLimitExceeded.scope(),
            ErrorScope::Request
        );
        assert_eq!(
            ErrorCode::SatelliteUnreachable.scope(),
            ErrorScope::Terminal
        );
        assert_eq!(ErrorCode::InternalError.scope(), ErrorScope::Terminal);
        assert_eq!(ErrorCode::WrongResourceKind.scope(), ErrorScope::Terminal);
        assert_eq!(ErrorCode::NotProducer.scope(), ErrorScope::Request);
        assert_eq!(ErrorCode::RecordInvalid.scope(), ErrorScope::Request);
        assert_eq!(ErrorCode::Overflow.scope(), ErrorScope::Request);
        assert_eq!(ErrorCode::PreconditionFailed.scope(), ErrorScope::Request);
        assert_eq!(ErrorCode::IncarnationChanged.scope(), ErrorScope::Request);
    }
}
