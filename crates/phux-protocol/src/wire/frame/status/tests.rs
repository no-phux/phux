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
    ErrorCode::Unsupported,
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
fn unallocated_codes_decode_as_unknown_and_round_trip() {
    for value in [0, 5, 8, 99, 214, 300, 65534] {
        let code = ErrorCode::from_wire_or_unknown(value);
        assert_eq!(code, ErrorCode::Unknown(value));
        assert_eq!(code.as_wire(), value);
        assert_eq!(code.scope(), ErrorScope::Request);
    }
    assert_eq!(ErrorCode::from_wire_or_unknown(7), ErrorCode::Unsupported);
    assert_eq!(ErrorCode::Unsupported.as_wire(), 7);
    assert_eq!(ErrorCode::Unsupported.scope(), ErrorScope::Request);
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
