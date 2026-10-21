//! Structured error codes and teardown/rejection reason enums (SPEC §14).

/// Structured error code carried by [`FrameKind::Error`](super::FrameKind::Error)
/// (SPEC §14), a big-endian `u16`.
///
/// Append-only: a newer peer may send a code this build does not know, so
/// an unallocated value decodes as [`Self::Unknown`] carrying the raw
/// value, never a frame error. Ranges: handshake `1..=9`, attach
/// `100..=199`, command `200..=299`, internal `u16::MAX`; each code's value
/// is in [`Self::as_wire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorCode {
    /// SPEC §6.1: HELLO version negotiation found no compatible version.
    VersionIncompatible,
    /// SPEC §6: the peer sent a type byte the receiver does not recognise.
    UnknownMessageType,
    /// SPEC §5 / Appendix A: a message could not be decoded.
    MalformedMessage,
    /// SPEC §5: a frame's declared length exceeded the protocol cap.
    FrameTooLarge,
    // Value 5 is permanently reserved for the withdrawn OUT_OF_TIER error.
    /// SPEC §6.1: peers share no usable explicit bootstrap profile/codec/features.
    CodecUnavailable,
    /// The receiver understood the frame's framing but not its content: a
    /// frame type, command tag, or enum value newer than its protocol 1.x
    /// build. Nothing happened and the connection stays open; the sender may
    /// fall back to an older shape.
    Unsupported,

    /// SPEC §13: the client issued an operation that requires an attach
    /// while not attached.
    NotAttached,
    /// SPEC §13: the client requested attach while already attached.
    AlreadyAttached,
    /// SPEC §13: the requested session does not exist.
    SessionNotFound,
    /// The requested window does not exist.
    WindowNotFound,
    /// The requested terminal does not exist.
    TerminalNotFound,
    /// The requested client id does not exist.
    ClientNotFound,
    /// A `Satellite` id reached a non-hub server, or a hub that does not
    /// know the host (ADR-0016).
    UnsupportedSatelliteRoute,
    /// The hub knows the satellite but its link is down; retryable
    /// (ADR-0007).
    SatelliteUnreachable,

    /// SPEC §11: the requested COMMAND payload was structurally invalid.
    InvalidCommand,
    /// SPEC §15: the requested operation is forbidden for this peer.
    PermissionDenied,
    /// The server has run out of a resource needed to satisfy the request
    /// (file descriptors, memory, PTYs, ...).
    ResourceExhausted,
    /// An untrusted paste in an atomic input batch failed the safety policy.
    UnsafePaste,
    /// A cooperative `ACQUIRE_INPUT` lost to the current lease holder
    /// (ADR-0033).
    InputLeaseHeld,
    /// Input reached the pane write path, but final PTY delivery is unknown.
    InputDeliveryUnknown,
    /// A canonical-mode (`ICANON`) line would exceed the kernel's limit and
    /// be truncated; nothing was written.
    CanonicalLimitExceeded,
    /// `APPLY_INPUT` provably never reached a live PTY writer, so a resubmit
    /// cannot type it twice.
    InputNotWritten,
    /// The verb does not apply to the subject's resource kind (a Terminal
    /// verb on an `AgentSession`, or an append to a Terminal).
    WrongResourceKind,
    /// `APPEND_RESOURCE_OUTPUT` from a client that is not the producer.
    NotProducer,
    /// `APPEND_RESOURCE_OUTPUT` bytes are not complete, valid codec records;
    /// nothing was appended.
    RecordInvalid,
    /// The append's retained ring or rate budget is exhausted; retry after
    /// backoff.
    Overflow,
    /// A `KILL_RESOURCE_IF` precondition did not hold (ADR-0109); nothing
    /// was killed.
    PreconditionFailed,
    /// A hub refused a keyed retry because the owning satellite restarted,
    /// so a replay could run twice (`docs/spec/L1.md` §9.1).
    IncarnationChanged,

    /// Catch-all for unexpected server-side failures.
    InternalError,

    /// A code this build does not know, with its raw wire value. Produced
    /// only by the decoder.
    Unknown(u16),
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
    /// Wire encoding of this code; [`Self::Unknown`] writes back its raw value.
    #[must_use]
    pub const fn as_wire(self) -> u16 {
        match self {
            Self::VersionIncompatible => 1,
            Self::UnknownMessageType => 2,
            Self::MalformedMessage => 3,
            Self::FrameTooLarge => 4,
            Self::CodecUnavailable => 6,
            Self::Unsupported => 7,
            Self::NotAttached => 100,
            Self::AlreadyAttached => 101,
            Self::SessionNotFound => 102,
            Self::WindowNotFound => 103,
            Self::TerminalNotFound => 104,
            Self::ClientNotFound => 105,
            Self::UnsupportedSatelliteRoute => 106,
            Self::SatelliteUnreachable => 107,
            Self::InvalidCommand => 200,
            Self::PermissionDenied => 201,
            Self::ResourceExhausted => 202,
            Self::UnsafePaste => 203,
            Self::InputLeaseHeld => 204,
            Self::InputDeliveryUnknown => 205,
            Self::CanonicalLimitExceeded => 206,
            Self::InputNotWritten => 207,
            Self::WrongResourceKind => 208,
            Self::NotProducer => 209,
            Self::RecordInvalid => 210,
            Self::Overflow => 211,
            Self::PreconditionFailed => 212,
            Self::IncarnationChanged => 213,
            Self::InternalError => 65535,
            Self::Unknown(value) => value,
        }
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
            | Self::IncarnationChanged
            | Self::Unsupported
            | Self::Unknown(_) => ErrorScope::Request,
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

    /// Inverse of [`Self::as_wire`] for the codes this build defines;
    /// `None` for any other value (never [`Self::Unknown`]).
    #[must_use]
    pub const fn from_wire(value: u16) -> Option<Self> {
        Some(match value {
            1 => Self::VersionIncompatible,
            2 => Self::UnknownMessageType,
            3 => Self::MalformedMessage,
            4 => Self::FrameTooLarge,
            6 => Self::CodecUnavailable,
            7 => Self::Unsupported,
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

    /// What a client settles a request with when the server's reply decoded
    /// as a well-framed `COMMAND_RESULT` but carried a result tag, value tag,
    /// or enum value this build does not know. The server answered, so the
    /// request may have taken effect; only the answer is unreadable. Unlike
    /// [`Self::Unsupported`], which promises nothing happened, it is an
    /// [`Self::Unknown`] code: outcome not stated. Wire value `0` is never
    /// allocated, so no server sends it.
    pub const UNREADABLE_REPLY: Self = Self::Unknown(0);

    /// Decode a wire value totally: a defined code, or [`Self::Unknown`]
    /// carrying the raw value. What every decoder uses.
    #[must_use]
    pub const fn from_wire_or_unknown(value: u16) -> Self {
        match Self::from_wire(value) {
            Some(code) => code,
            None => Self::Unknown(value),
        }
    }
}
wire_enum! { as_wire / from_wire;
/// Why a bootstrap generation can no longer preserve stream continuity.
///
/// A value this build does not define decodes as [`Self::Unknown`], never a
/// frame error: the generation is retired either way.
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
    /// A reason this build does not know. Produced only by the decoder.
    /// `255` is reserved for it: no reason is ever allocated that byte, so
    /// relaying an unknown reason as `255` can never claim a real one.
    Unknown = 255,
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
    /// `255` is reserved for it and never allocated to a reason.
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
///
/// A value this build does not define decodes as [`Self::Unknown`], never a
/// frame error: the cursor is gone either way.
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
    /// A reason this build does not know. Produced only by the decoder.
    /// `255` is reserved for it: no reason is ever allocated that byte, so
    /// relaying an unknown reason as `255` can never claim a real one.
    Unknown = 255,
}
}

wire_enum! { as_wire / from_wire;
/// Why one history request was rejected without advancing its cursor.
///
/// A value this build does not define decodes as [`Self::Unknown`], never a
/// frame error; the cursor is still not advanced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HistoryRejectionReason {
    /// A required byte or row request limit was zero.
    ZeroLimit = 0,
    /// The requested limits cannot fit the next independently decodable unit.
    TooSmall = 1,
    /// Capture is temporarily busy; retrying the same cursor is permitted.
    Busy = 2,
    /// A reason this build does not know. Produced only by the decoder.
    /// `255` is reserved for it: no reason is ever allocated that byte, so
    /// relaying an unknown reason as `255` can never claim a real one.
    Unknown = 255,
}
}

#[cfg(test)]
mod tests;
