//! Structured error codes and teardown/rejection reason enums (SPEC §14).

// -----------------------------------------------------------------------------
// ErrorCode enum — SPEC §14.
// -----------------------------------------------------------------------------

/// Structured error code carried by [`FrameKind::Error`](super::FrameKind::Error), per SPEC §14.
///
/// Marked `#[non_exhaustive]` so future minor protocol versions can add
/// codes without breaking downstream matches (per the protocol/core
/// independence principle in ADR-0011). Unknown wire values surface as
/// [`DecodeError::UnknownEnumValue`](crate::wire::error::DecodeError::UnknownEnumValue) rather than being silently mapped to
/// a placeholder variant — misinterpreting an error code can mask the
/// underlying failure.
///
/// The numeric values are the wire encoding: `u16` big-endian. The space
/// is intentionally sparse (handshake errors clustered at `1..=9`,
/// attach/session at `100..=199`, command errors at `200..=299`, internal
/// at `u16::MAX`) so future codes can slot in without renumbering.
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
    /// SPEC §10.1 / ADR-0016: the frame carried a `ResourceId::Satellite`
    /// but this server is not configured as a federation hub, or names a
    /// satellite host absent from the hub's registry. Non-hub servers
    /// always respond with this code when handed a `Satellite` id.
    UnsupportedSatelliteRoute = 106,
    /// ADR-0007 / SPEC §14: the frame's `ResourceId::Satellite` names a
    /// satellite this hub knows but cannot reach right now — the outbound
    /// link is down, still dialing, refused fail-closed, or dropped before
    /// the relayed reply arrived. Distinct from
    /// [`Self::UnsupportedSatelliteRoute`] (a routing/configuration
    /// refusal): this one is transient and a retry may succeed once the
    /// link supervisor reconnects.
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
    /// ADR-0033: a cooperative `ACQUIRE_INPUT` was refused because another
    /// client already holds the Terminal's input lease. The diagnostic names
    /// the current holder. A `Seize`-mode acquire never surfaces this — it
    /// preempts the holder instead. (`203` is reserved for `UNSAFE_PASTE` in
    /// SPEC §14, so this takes `204`.)
    InputLeaseHeld = 204,
    /// Input reached the pane write path, but final PTY delivery is unknown.
    InputDeliveryUnknown = 205,
    /// phux-mjmc: the pane's line discipline is in canonical mode
    /// (`ICANON`) and the input batch's encoded PTY bytes contain a line
    /// longer than the pane's canonical-line limit with no terminator to
    /// flush it. Writing it would have silently truncated at the kernel's
    /// canonical queue boundary instead of delivering it — refused before
    /// any bytes reached the pane. Distinct from [`Self::InputDeliveryUnknown`]:
    /// that code means delivery could not be *confirmed*; this one means
    /// delivery is *known* to be unsafe and nothing was written.
    CanonicalLimitExceeded = 206,
    /// phux-w7z2.60: `APPLY_INPUT` was refused, or its pending write was
    /// abandoned, at a point the server can **prove** never reached a live
    /// PTY writer — the pane has no PTY, the writer's queue was full, the
    /// writer's channel was already closed, or the pane's own actor had
    /// already gone before handoff. Distinct from
    /// [`Self::InputDeliveryUnknown`] the same way
    /// [`Self::CanonicalLimitExceeded`] is: that code means delivery could
    /// not be *confirmed*; this one means delivery is *known* never to have
    /// been attempted, so resubmitting the batch — under the same operation
    /// id or a fresh one — cannot type it twice.
    InputNotWritten = 207,
    /// The command's subject is a resource of a kind that does not support
    /// the operation: a Terminal-facet verb (input, resize, screen, history,
    /// lease, signal, upload, transcription) aimed at an `AgentSession`, or
    /// `APPEND_RESOURCE_OUTPUT` aimed at a Terminal. The resource exists;
    /// the verb does not apply to it. Terminal-scoped like
    /// [`Self::TerminalNotFound`]: the consumer keeps every other resource
    /// and its attach.
    WrongResourceKind = 208,
    /// `APPEND_RESOURCE_OUTPUT` was sent by a client that is not the
    /// resource's producer. Only the producer that opened a producer-fed
    /// resource may append to its output.
    NotProducer = 209,
    /// `APPEND_RESOURCE_OUTPUT` carried bytes that are not one or more
    /// complete, well-formed records under the resource's codec (for
    /// `AgentEventsJsonlV1`: not UTF-8 JSON lines, an unknown record `type`,
    /// or a record over its per-record bound). Nothing was appended.
    RecordInvalid = 210,
    /// `APPEND_RESOURCE_OUTPUT` was refused because the resource's retained
    /// output ring or rate budget is exhausted. Nothing was appended; the
    /// producer may retry after backing off.
    Overflow = 211,
    /// `KILL_RESOURCE_IF` (ADR-0109): a precondition did not hold, or could
    /// not be established, so nothing was killed. The instance token no
    /// longer names this server's id space, the resource has been attached
    /// by a connection other than the one that spawned it, a condition bit
    /// is unknown, or a hub cannot vouch for a satellite's resource. The
    /// message says which. Every case has the same recovery: leave the
    /// resource alone.
    PreconditionFailed = 212,

    /// Catch-all for unexpected server-side failures. Carries
    /// `u16::MAX = 65535` on the wire.
    InternalError = 65535,
}

/// How much of a consumer's world an [`ErrorCode`] invalidates.
///
/// A consumer never learns fatality from a code — the same code is emitted
/// both fatally and non-fatally by the same server, and SPEC §9 makes
/// termination the job of `DETACHED` plus transport close, never of an
/// `ERROR`. What the scope answers is the narrower question the consumer
/// actually has to answer on receipt: how far to degrade.
///
/// This is a Rust-side classification, not a wire field. `FrameKind::Error`
/// carries no terminal id, so an uncorrelated `Terminal`-scoped error cannot
/// be attributed to one pane; the scope is what the consumer knows about the
/// blast radius, not about the subject.
///
/// Marked `#[non_exhaustive]` for the same reason [`ErrorCode`] is: a later
/// protocol version may need a scope this one does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorScope {
    /// One Terminal's work failed. Everything else the consumer holds —
    /// the other panes, the layout, the attach — remains valid.
    Terminal,
    /// One request failed. The consumer's request table owns the outcome;
    /// no projected state changes.
    Request,
    /// The connection itself is unusable, so the consumer should **expect
    /// the server to close it** — a `DETACHED { PROTOCOL_ERROR }` and a
    /// transport close, per SPEC §9's sender obligation. It does **not**
    /// mean "close now": the consumer keeps reading until the close
    /// arrives, so any frames still in flight (including the `DETACHED`
    /// itself) are observed rather than discarded.
    Connection,
}

impl ErrorCode {
    /// Wire encoding of this code: the `#[repr(u16)]` discriminant.
    #[must_use]
    pub const fn as_wire(self) -> u16 {
        self as u16
    }

    /// How far a consumer should degrade on receipt of this code; see
    /// [`ErrorScope`].
    ///
    /// The match is deliberately exhaustive with no wildcard arm, so adding
    /// an [`ErrorCode`] variant is a compile error here rather than a silent
    /// default that misclassifies the new code's blast radius.
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
            | Self::PreconditionFailed => ErrorScope::Request,
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
            65535 => Self::InternalError,
            _ => return None,
        })
    }
}
/// Why a bootstrap generation can no longer preserve stream continuity.
#[repr(u8)]
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

impl TombstoneReason {
    /// Stable wire discriminant.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        self as u8
    }

    /// Decode a known tombstone reason.
    #[must_use]
    pub const fn from_wire(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::RawReplayOverflow,
            1 => Self::OutboundGap,
            2 => Self::Resize,
            3 => Self::RelayReconnect,
            4 => Self::ExplicitReattach,
            5 => Self::CodecFailure,
            6 => Self::Other,
            _ => return None,
        })
    }
}

/// Why the server ended an attach (`docs/spec/proto.md` §7.2).
///
/// Carried by `DETACHED`, which — with the transport close that follows it —
/// is the only ending a consumer is allowed to act on. An `ERROR` is never
/// itself an ending (proto.md §9), so this enum is the sole channel through
/// which a consumer learns *why* its connection is over.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DetachReason {
    /// The consumer asked: its own `DETACH`, or an operator's
    /// `DETACH_CLIENTS` sweep on its behalf.
    Requested = 0,
    /// The server process is stopping (`SHUTDOWN`, signal, or supervisor).
    ServerShutdown = 1,
    /// Legacy name, retained for wire compatibility: the group the attach was
    /// rooted in was torn down (now a `KILL_RESOURCES` over its members; see
    /// `docs/spec/L2.md` / ADR-0030).
    SessionKilled = 2,
    /// Another consumer took over an exclusive attach.
    Replaced = 3,
    /// The peer violated the protocol; the sender is closing the transport.
    /// A fatal `ERROR` MUST be followed by `DETACHED` carrying this reason.
    ProtocolError = 4,
    /// The server hit an unrecoverable internal fault.
    InternalError = 255,
}

impl DetachReason {
    /// Stable wire discriminant.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        self as u8
    }

    /// Decode a known detach reason, or `None` for a value this build does
    /// not recognise.
    ///
    /// Unlike [`TombstoneReason`] and [`ErrorCode`], an unrecognised value
    /// here is deliberately *not* a decode error. `DETACHED` is the
    /// termination signal; failing the frame would convert a clean, explained
    /// ending into an unexplained transport error, and would make every later
    /// `DetachReason` allocation a fleet-wide break (ADR-0061). Callers treat
    /// `None` exactly as they treat an absent `reason` field: unstated.
    #[must_use]
    pub const fn from_wire(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Requested,
            1 => Self::ServerShutdown,
            2 => Self::SessionKilled,
            3 => Self::Replaced,
            4 => Self::ProtocolError,
            255 => Self::InternalError,
            _ => return None,
        })
    }

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
            Self::InternalError => "the server hit an internal error",
        }
    }
}

/// Why a resource ceased to exist (`RESOURCE_CLOSED.reason`,
/// `docs/spec/L1.md` §3.1).
///
/// Carried as an optional trailing field, so a body that predates the field
/// decodes as [`Unknown`](Self::Unknown) and a body carrying `Unknown` is
/// encoded with the field absent. Like [`DetachReason`], an unrecognised
/// wire value is not a decode error: `RESOURCE_CLOSED` is a lifecycle fact
/// and failing it would hide the ending, so a future reason decodes as
/// `Unknown` on an older peer.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum CloseReason {
    /// The backing process exited on its own (PTY EOF, `_exit`, or a signal
    /// the process received from something other than phux), or a
    /// producer-fed resource was closed by its producer.
    Exited = 0,
    /// A `KILL_RESOURCE` / `KILL_RESOURCES` command removed the resource.
    Killed = 1,
    /// The resource's parent closed, and the server cascaded the close under
    /// its single state lock.
    ParentClosed = 2,
    /// The server process is stopping and reaped the resource on the way
    /// out.
    ServerShutdown = 3,
    /// The server stated no reason, or stated one this build does not
    /// recognise. Encoded as an absent field.
    #[default]
    Unknown = 255,
}

impl CloseReason {
    /// Stable wire discriminant. `Unknown` has none: an encoder omits the
    /// field instead, so callers check [`Self::is_unknown`] first.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        self as u8
    }

    /// Decode a wire value. An unrecognised value is [`Self::Unknown`], never
    /// an error, for the reason given on the type.
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

    /// `true` iff the reason is unstated, which the encoder expresses as an
    /// absent field.
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

/// Why one progressive history cursor can no longer be consumed.
#[repr(u8)]
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

impl HistoryTombstoneReason {
    /// Stable wire discriminant.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        self as u8
    }

    /// Decode a known history-tombstone reason.
    #[must_use]
    pub const fn from_wire(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Stale,
            1 => Self::Pruned,
            2 => Self::Reset,
            3 => Self::Resize,
            4 => Self::Expired,
            5 => Self::Released,
            6 => Self::Limit,
            7 => Self::CodecFailure,
            _ => return None,
        })
    }
}

/// Why one history request was rejected without advancing its cursor.
#[repr(u8)]
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

impl HistoryRejectionReason {
    /// Stable wire discriminant.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        self as u8
    }

    /// Decode a known history-rejection reason.
    #[must_use]
    pub const fn from_wire(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::ZeroLimit,
            1 => Self::TooSmall,
            2 => Self::Busy,
            _ => return None,
        })
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
        ErrorCode::InternalError,
    ];

    #[test]
    fn every_error_code_round_trips_and_carries_a_scope() {
        for &code in ALL_CODES {
            assert_eq!(
                ErrorCode::from_wire(code.as_wire()),
                Some(code),
                "{code:?} does not round-trip through the wire tables"
            );
            assert!(
                matches!(
                    code.scope(),
                    ErrorScope::Terminal | ErrorScope::Request | ErrorScope::Connection
                ),
                "{code:?} has no scope"
            );
        }
    }

    #[test]
    fn the_decodable_wire_space_is_exactly_the_known_codes() {
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
    }
}
