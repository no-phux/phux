//! History pages, requests, status frames, and their negotiated bounds.

use super::{
    Bytes, BytesMut, DecodeError, FrameKind, HistoryRejectionReason, HistoryTombstoneReason,
    MAX_BOOTSTRAP_CHUNK_BYTES, MAX_HISTORY_CURSOR_BYTES, MAX_HISTORY_PAGE_BYTES,
    MAX_HISTORY_PAGE_ROWS, ResourceId, TYPE_BOOTSTRAP_BEGIN, TYPE_HISTORY_PAGE,
    TYPE_HISTORY_REJECTED, TYPE_HISTORY_TOMBSTONE, assert_round_trip, bootstrap,
    encode_without_field, framed, local_terminal, stream, tlv_field,
};

#[test]
fn history_page_sequence_and_row_count_are_required() {
    let page = FrameKind::HistoryPage {
        terminal_id: ResourceId::local(1),
        stream_id: stream(2),
        bootstrap_id: bootstrap(3),
        page_seq: 1,
        cursor: Bytes::from_static(b"stable-lease"),
        next_cursor: None,
        payload: Bytes::from_static(b"authenticated-finish"),
        rows: 0,
    };
    for omitted_id in [7, 8] {
        assert_eq!(
            FrameKind::decode(&encode_without_field(&page, omitted_id)).unwrap_err(),
            DecodeError::UnexpectedEof
        );
    }

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
    let request = FrameKind::HistoryRequest {
        terminal_id: ResourceId::local(1),
        stream_id: stream(2),
        bootstrap_id: bootstrap(3),
        cursor: Bytes::from_static(b"stable-lease"),
        max_bytes: 4096,
        max_rows: 1024,
    };
    for omitted_id in [5, 6] {
        assert_eq!(
            FrameKind::decode(&encode_without_field(&request, omitted_id)).unwrap_err(),
            DecodeError::UnexpectedEof
        );
    }
}

#[test]
fn zero_history_request_limits_decode_for_retryable_rejection() {
    assert_round_trip(&FrameKind::HistoryRequest {
        terminal_id: ResourceId::local(1),
        stream_id: stream(2),
        bootstrap_id: bootstrap(3),
        cursor: Bytes::from_static(b"retryable-cursor"),
        max_bytes: 0,
        max_rows: 0,
    });
}

#[test]
fn history_status_enums_round_trip_and_unknown_tags_are_rejected() {
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
        assert_round_trip(&FrameKind::HistoryTombstone {
            terminal_id: ResourceId::local(1),
            stream_id: stream(2),
            bootstrap_id: bootstrap(3),
            cursor: Bytes::from_static(b"cursor"),
            reason,
        });
    }
    for (tag, reason) in [
        (0, HistoryRejectionReason::ZeroLimit),
        (1, HistoryRejectionReason::TooSmall),
        (2, HistoryRejectionReason::Busy),
    ] {
        assert_eq!(reason.as_wire(), tag);
        assert_round_trip(&FrameKind::HistoryRejected {
            terminal_id: ResourceId::local(1),
            stream_id: stream(2),
            bootstrap_id: bootstrap(3),
            cursor: Bytes::from_static(b"cursor"),
            reason,
            required_bytes: 4096,
            required_rows: 256,
        });
    }

    for type_byte in [TYPE_HISTORY_TOMBSTONE, TYPE_HISTORY_REJECTED] {
        let mut fields = Vec::new();
        tlv_field(&mut fields, 1, &local_terminal(1));
        tlv_field(&mut fields, 2, &2_u64.to_be_bytes());
        tlv_field(&mut fields, 3, &3_u64.to_be_bytes());
        tlv_field(&mut fields, 4, b"cursor");
        tlv_field(&mut fields, 5, &[0xff]);
        if type_byte == TYPE_HISTORY_REJECTED {
            tlv_field(&mut fields, 6, &4096_u32.to_be_bytes());
            tlv_field(&mut fields, 7, &256_u32.to_be_bytes());
        }
        assert!(matches!(
            FrameKind::decode(&framed(type_byte, &fields)).unwrap_err(),
            DecodeError::UnknownEnumValue { .. }
        ));
    }
}

#[test]
fn history_status_fields_and_retry_bounds_are_enforced() {
    let tombstone = FrameKind::HistoryTombstone {
        terminal_id: ResourceId::local(1),
        stream_id: stream(2),
        bootstrap_id: bootstrap(3),
        cursor: Bytes::from_static(b"cursor"),
        reason: HistoryTombstoneReason::Stale,
    };
    for omitted_id in 1..=5 {
        assert_eq!(
            FrameKind::decode(&encode_without_field(&tombstone, omitted_id)).unwrap_err(),
            DecodeError::UnexpectedEof
        );
    }

    let rejected = FrameKind::HistoryRejected {
        terminal_id: ResourceId::local(1),
        stream_id: stream(2),
        bootstrap_id: bootstrap(3),
        cursor: Bytes::from_static(b"cursor"),
        reason: HistoryRejectionReason::TooSmall,
        required_bytes: 4096,
        required_rows: 256,
    };
    for omitted_id in 1..=7 {
        assert_eq!(
            FrameKind::decode(&encode_without_field(&rejected, omitted_id)).unwrap_err(),
            DecodeError::UnexpectedEof
        );
    }

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
        let invalid = FrameKind::HistoryRejected {
            terminal_id: ResourceId::local(1),
            stream_id: stream(2),
            bootstrap_id: bootstrap(3),
            cursor: Bytes::from_static(b"cursor"),
            reason: HistoryRejectionReason::TooSmall,
            required_bytes,
            required_rows,
        };
        let mut encoded = BytesMut::new();
        invalid.encode(&mut encoded);
        assert_eq!(FrameKind::decode(&encoded).unwrap_err(), expected);
    }
}

#[test]
fn hard_response_bounds_are_enforced_and_request_limits_reach_host_for_clamping() {
    let over_chunk = FrameKind::BootstrapChunk {
        terminal_id: ResourceId::local(1),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        chunk_seq: 0,
        payload: Bytes::from(vec![0; MAX_BOOTSTRAP_CHUNK_BYTES as usize + 1]),
    };
    let mut encoded = BytesMut::new();
    over_chunk.encode(&mut encoded);
    assert_eq!(
        FrameKind::decode(&encoded).unwrap_err(),
        DecodeError::BootstrapLimitExceeded
    );

    let over_page = FrameKind::HistoryPage {
        terminal_id: ResourceId::local(1),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        page_seq: 1,
        cursor: Bytes::from_static(b"cursor"),
        next_cursor: None,
        payload: Bytes::from(vec![0; MAX_HISTORY_PAGE_BYTES as usize + 1]),
        rows: 1,
    };
    encoded.clear();
    over_page.encode(&mut encoded);
    assert_eq!(
        FrameKind::decode(&encoded).unwrap_err(),
        DecodeError::BootstrapLimitExceeded
    );

    let over_cursor = FrameKind::BootstrapReady {
        terminal_id: ResourceId::local(1),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        history_cursor: Some(Bytes::from(vec![0; MAX_HISTORY_CURSOR_BYTES + 1])),
    };
    encoded.clear();
    over_cursor.encode(&mut encoded);
    assert_eq!(
        FrameKind::decode(&encoded).unwrap_err(),
        DecodeError::BootstrapLimitExceeded
    );

    let over_bytes = FrameKind::HistoryRequest {
        terminal_id: ResourceId::local(1),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        cursor: Bytes::from_static(b"cursor"),
        max_bytes: MAX_HISTORY_PAGE_BYTES + 1,
        max_rows: 1024,
    };
    assert_round_trip(&over_bytes);

    let over_rows = FrameKind::HistoryRequest {
        terminal_id: ResourceId::local(1),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        cursor: Bytes::from_static(b"cursor"),
        max_bytes: 4096,
        max_rows: MAX_HISTORY_PAGE_ROWS + 1,
    };
    assert_round_trip(&over_rows);

    let over_page_rows = FrameKind::HistoryPage {
        terminal_id: ResourceId::local(1),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        page_seq: 1,
        cursor: Bytes::from_static(b"cursor"),
        next_cursor: None,
        payload: Bytes::from_static(b"finish"),
        rows: MAX_HISTORY_PAGE_ROWS + 1,
    };
    encoded.clear();
    over_page_rows.encode(&mut encoded);
    assert_eq!(
        FrameKind::decode(&encoded).unwrap_err(),
        DecodeError::HistoryRowLimitExceeded
    );
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
