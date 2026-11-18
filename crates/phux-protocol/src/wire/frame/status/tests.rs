use super::{
    CloseReason, ErrorCode, ErrorScope, HistoryRejectionReason, HistoryTombstoneReason,
    TombstoneReason,
};

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

#[test]
fn unallocated_codes_decode_as_unknown_and_round_trip() {
    for value in [0, 5, 8, 99, 216, 300, 65534] {
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
    assert_eq!(ErrorCode::NotFound.scope(), ErrorScope::Request);
    assert_eq!(ErrorCode::Unavailable.scope(), ErrorScope::Request);
}

/// `255` is the `Unknown` byte of every reason enum, never a real reason:
/// only `255` decodes as `Unknown`, and every known reason encodes below it.
#[test]
fn byte_255_is_reserved_for_unknown_reasons() {
    assert_eq!(
        TombstoneReason::from_wire(255),
        Some(TombstoneReason::Unknown)
    );
    assert_eq!(
        HistoryTombstoneReason::from_wire(255),
        Some(HistoryTombstoneReason::Unknown)
    );
    assert_eq!(
        HistoryRejectionReason::from_wire(255),
        Some(HistoryRejectionReason::Unknown)
    );
    for value in 0..255 {
        assert_ne!(
            TombstoneReason::from_wire(value),
            Some(TombstoneReason::Unknown)
        );
        assert_ne!(
            HistoryTombstoneReason::from_wire(value),
            Some(HistoryTombstoneReason::Unknown)
        );
        assert_ne!(
            HistoryRejectionReason::from_wire(value),
            Some(HistoryRejectionReason::Unknown)
        );
    }
    assert_eq!(CloseReason::Unknown.as_wire(), 255);
}
