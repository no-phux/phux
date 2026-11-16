//! History pages, requests, status frames, and their negotiated bounds.

use super::{
    Bytes, DecodeError, FrameKind, HistoryRejectionReason, HistoryTombstoneReason,
    MAX_BOOTSTRAP_CHUNK_BYTES, MAX_HISTORY_CURSOR_BYTES, MAX_HISTORY_PAGE_BYTES,
    MAX_HISTORY_PAGE_ROWS, ResourceId, TYPE_BOOTSTRAP_BEGIN, TYPE_HISTORY_PAGE,
    TYPE_HISTORY_REJECTED, TYPE_HISTORY_TOMBSTONE, assert_round_trip, bootstrap, chunk, decode_err,
    encode_without_field, framed, history_page, history_rejected, history_request,
    history_tombstone, local_terminal, stream, tlv_field,
};

/// Omitting any of `ids` from `frame`'s encoding is a truncated frame.
fn assert_fields_required(what: &str, frame: &FrameKind, ids: impl IntoIterator<Item = u64>) {
    for omitted_id in ids {
        assert_eq!(
            FrameKind::decode(&encode_without_field(frame, omitted_id)).unwrap_err(),
            DecodeError::UnexpectedEof,
            "{what} without field {omitted_id}"
        );
    }
}

#[test]
fn history_page_sequence_and_row_count_are_required() {
    let mut page = history_page(Bytes::from_static(b"authenticated-finish"), 0);
    if let FrameKind::HistoryPage { cursor, .. } = &mut page {
        *cursor = Bytes::from_static(b"stable-lease");
    }
    assert_fields_required("history page", &page, [7, 8]);

    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &local_terminal(1));
    tlv_field(&mut fields, 2, &2_u64.to_be_bytes());
    tlv_field(&mut fields, 3, &3_u64.to_be_bytes());
    tlv_field(&mut fields, 4, b"stable-lease");
    tlv_field(&mut fields, 6, b"authenticated-finish");
    tlv_field(&mut fields, 7, &0_u64.to_be_bytes());
    tlv_field(&mut fields, 8, &0_u32.to_be_bytes());
    assert_eq!(
        FrameKind::decode(&framed(TYPE_HISTORY_PAGE, &fields)).unwrap_err(),
        DecodeError::InvalidHistoryPageSequence
    );
    let request = history_request(b"stable-lease", 4096, 1024);
    assert_fields_required("history request", &request, [5, 6]);
}

#[test]
fn zero_history_request_limits_decode_for_retryable_rejection() {
    assert_round_trip(&history_request(b"retryable-cursor", 0, 0));
}

#[test]
fn history_status_enums_round_trip_and_unknown_tags_decode_as_unknown() {
    for (tag, reason) in [
        (0, HistoryTombstoneReason::Stale),
        (1, HistoryTombstoneReason::Pruned),
        (2, HistoryTombstoneReason::Reset),
        (3, HistoryTombstoneReason::Resize),
        (4, HistoryTombstoneReason::Expired),
        (5, HistoryTombstoneReason::Released),
        (6, HistoryTombstoneReason::Limit),
        (7, HistoryTombstoneReason::CodecFailure),
    ] {
        assert_eq!(reason.as_wire(), tag);
        assert_round_trip(&history_tombstone(reason));
    }
    for (tag, reason) in [
        (0, HistoryRejectionReason::ZeroLimit),
        (1, HistoryRejectionReason::TooSmall),
        (2, HistoryRejectionReason::Busy),
    ] {
        assert_eq!(reason.as_wire(), tag);
        assert_round_trip(&history_rejected(reason, 4096, 256));
    }

    for type_byte in [TYPE_HISTORY_TOMBSTONE, TYPE_HISTORY_REJECTED] {
        let mut fields = Vec::new();
        tlv_field(&mut fields, 1, &local_terminal(1));
        tlv_field(&mut fields, 2, &2_u64.to_be_bytes());
        tlv_field(&mut fields, 3, &3_u64.to_be_bytes());
        tlv_field(&mut fields, 4, b"cursor");
        tlv_field(&mut fields, 5, &[0xfe]);
        if type_byte == TYPE_HISTORY_REJECTED {
            tlv_field(&mut fields, 6, &4096_u32.to_be_bytes());
            tlv_field(&mut fields, 7, &256_u32.to_be_bytes());
        }
        let reason_known = match FrameKind::decode(&framed(type_byte, &fields)).unwrap().0 {
            FrameKind::HistoryTombstone { reason, .. } => reason == HistoryTombstoneReason::Unknown,
            FrameKind::HistoryRejected { reason, .. } => reason == HistoryRejectionReason::Unknown,
            other => panic!("unexpected {other:?}"),
        };
        assert!(reason_known, "a newer server's reason decodes as Unknown");
    }
}

#[test]
fn history_status_fields_and_retry_bounds_are_enforced() {
    let tombstone = history_tombstone(HistoryTombstoneReason::Stale);
    assert_fields_required("history tombstone", &tombstone, 1..=5);
    let rejected = history_rejected(HistoryRejectionReason::TooSmall, 4096, 256);
    assert_fields_required("history rejected", &rejected, 1..=7);

    for (required_bytes, required_rows, expected) in [
        (0, 256, DecodeError::BootstrapLimitExceeded),
        (4096, 0, DecodeError::HistoryRowLimitExceeded),
        (
            MAX_HISTORY_PAGE_BYTES + 1,
            256,
            DecodeError::BootstrapLimitExceeded,
        ),
        (
            4096,
            MAX_HISTORY_PAGE_ROWS + 1,
            DecodeError::HistoryRowLimitExceeded,
        ),
    ] {
        let invalid = history_rejected(
            HistoryRejectionReason::TooSmall,
            required_bytes,
            required_rows,
        );
        assert_eq!(
            decode_err(&invalid),
            expected,
            "required {required_bytes} bytes / {required_rows} rows"
        );
    }
}

#[test]
fn hard_response_bounds_are_enforced_and_request_limits_reach_host_for_clamping() {
    let over_ready = FrameKind::BootstrapReady {
        terminal_id: ResourceId::local(1),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        history_cursor: Some(Bytes::from(vec![0; MAX_HISTORY_CURSOR_BYTES + 1])),
    };
    for (what, frame, expected) in [
        (
            "chunk over the payload cap",
            chunk(Bytes::from(vec![0; MAX_BOOTSTRAP_CHUNK_BYTES as usize + 1])),
            DecodeError::BootstrapLimitExceeded,
        ),
        (
            "page over the payload cap",
            history_page(Bytes::from(vec![0; MAX_HISTORY_PAGE_BYTES as usize + 1]), 1),
            DecodeError::BootstrapLimitExceeded,
        ),
        (
            "ready over the cursor cap",
            over_ready,
            DecodeError::BootstrapLimitExceeded,
        ),
        (
            "page over the row cap",
            history_page(Bytes::from_static(b"finish"), MAX_HISTORY_PAGE_ROWS + 1),
            DecodeError::HistoryRowLimitExceeded,
        ),
    ] {
        assert_eq!(decode_err(&frame), expected, "{what}");
    }

    // Request budgets over the caps still decode; the host clamps them.
    assert_round_trip(&history_request(
        b"cursor",
        MAX_HISTORY_PAGE_BYTES + 1,
        1024,
    ));
    assert_round_trip(&history_request(b"cursor", 4096, MAX_HISTORY_PAGE_ROWS + 1));
}

#[test]
fn malformed_native_state_sync_combination_is_rejected() {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &local_terminal(1));
    tlv_field(&mut fields, 2, &1u64.to_be_bytes());
    tlv_field(&mut fields, 3, &1u64.to_be_bytes());
    tlv_field(&mut fields, 4, &[1, 2]); // Native, checkpoint v2.
    tlv_field(&mut fields, 5, &80u16.to_be_bytes());
    tlv_field(&mut fields, 6, &24u16.to_be_bytes());
    tlv_field(&mut fields, 7, &[1]); // StateSync is illegal with native.
    tlv_field(&mut fields, 8, &0u64.to_be_bytes());
    assert_eq!(
        FrameKind::decode(&framed(TYPE_BOOTSTRAP_BEGIN, &fields)).unwrap_err(),
        DecodeError::InvalidBootstrapProfile
    );
}
