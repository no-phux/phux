//! Focused protocol-0.7 bootstrap/profile wire contract tests.

#![allow(clippy::unwrap_used)]

use bytes::{Bytes, BytesMut};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfile, BootstrapProfileKind,
    BootstrapProfileSet, BootstrapStreamProfile, ClientCapabilities, EngineCodec, EngineCodecSet,
    EngineFeature, EngineFeatureSet, MAX_BOOTSTRAP_CHUNK_BYTES, MAX_HISTORY_PAGE_BYTES, OutputMode,
    ServerCapabilities, select_bootstrap_profile,
};
use phux_protocol::ids::{BootstrapId, StreamId, TerminalId};
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::{
    FrameKind, HistoryRejectionReason, HistoryTombstoneReason, MAX_HISTORY_CURSOR_BYTES,
    MAX_HISTORY_PAGE_ROWS, TYPE_ATTACH_READY, TYPE_BOOTSTRAP_BEGIN, TYPE_BOOTSTRAP_CHUNK,
    TYPE_BOOTSTRAP_READY, TYPE_BOOTSTRAP_TOMBSTONE, TYPE_HELLO_OK, TYPE_HISTORY_PAGE,
    TYPE_HISTORY_REJECTED, TYPE_HISTORY_REQUEST, TYPE_HISTORY_TOMBSTONE, TombstoneReason,
};

fn stream(raw: u64) -> StreamId {
    StreamId::new(raw).unwrap()
}

fn bootstrap(raw: u64) -> BootstrapId {
    BootstrapId::new(raw).unwrap()
}

fn round_trip(frame: FrameKind) {
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn tlv_field(out: &mut Vec<u8>, id: u32, value: &[u8]) {
    put_varint(out, u64::from(id));
    out.push(4);
    put_varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

fn framed(type_byte: u8, fields: &[u8]) -> Vec<u8> {
    let length = 1usize.checked_add(fields.len()).unwrap();
    let mut out = Vec::with_capacity(length + 4);
    out.extend_from_slice(&u32::try_from(length).unwrap().to_be_bytes());
    out.push(type_byte);
    out.extend_from_slice(fields);
    out
}

fn take_varint(input: &[u8], offset: &mut usize) -> u64 {
    let mut value = 0u64;
    let mut shift = 0;
    loop {
        let byte = input[*offset];
        *offset += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return value;
        }
        shift += 7;
    }
}

fn encoded_field_ids(frame: &FrameKind) -> Vec<u64> {
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    let mut offset = 5;
    let mut ids = Vec::new();
    while offset < encoded.len() {
        ids.push(take_varint(&encoded, &mut offset));
        assert_eq!(encoded[offset], 4);
        offset += 1;
        let length = usize::try_from(take_varint(&encoded, &mut offset)).unwrap();
        offset += length;
    }
    assert_eq!(offset, encoded.len());
    ids
}

fn encode_without_field(frame: &FrameKind, omitted_id: u64) -> Vec<u8> {
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    let type_byte = encoded[4];
    let mut offset = 5;
    let mut fields = Vec::new();
    while offset < encoded.len() {
        let field_start = offset;
        let id = take_varint(&encoded, &mut offset);
        assert_eq!(encoded[offset], 4);
        offset += 1;
        let length = usize::try_from(take_varint(&encoded, &mut offset)).unwrap();
        offset += length;
        if id != omitted_id {
            fields.extend_from_slice(&encoded[field_start..offset]);
        }
    }
    framed(type_byte, &fields)
}

fn local_terminal(raw: u32) -> [u8; 5] {
    let mut value = [0u8; 5];
    value[1..].copy_from_slice(&raw.to_be_bytes());
    value
}

#[test]
fn protocol_07_discriminants_are_exact_and_snapshot_slot_is_retired() {
    assert_eq!(TYPE_HISTORY_REQUEST, 0x16);
    assert_eq!(TYPE_ATTACH_READY, 0x83);
    assert_eq!(TYPE_BOOTSTRAP_BEGIN, 0x93);
    assert_eq!(TYPE_BOOTSTRAP_CHUNK, 0x94);
    assert_eq!(TYPE_BOOTSTRAP_READY, 0x95);
    assert_eq!(TYPE_HISTORY_PAGE, 0x96);
    assert_eq!(TYPE_BOOTSTRAP_TOMBSTONE, 0x97);
    assert_eq!(TYPE_HISTORY_TOMBSTONE, 0x98);
    assert_eq!(TYPE_HISTORY_REJECTED, 0x99);
    assert_eq!(BootstrapProfileKind::NativeState as u8, 0x08);
    assert_eq!(BootstrapProfile::NATIVE_STATE_TAG, 3);
    assert_eq!(BootstrapProfileSet::from_wire(0x01).as_wire(), 0);

    let retired = [0, 0, 0, 1, 0x91];
    assert_eq!(
        FrameKind::decode(&retired).unwrap_err(),
        DecodeError::UnknownFrameKind { tag: 0x91 }
    );
}

#[test]
fn every_bootstrap_history_and_generation_frame_round_trips() {
    let terminal_id = TerminalId::local(42);
    let stream_id = stream(7);
    let bootstrap_id = bootstrap(9);

    round_trip(FrameKind::BootstrapBegin {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        profile: BootstrapStreamProfile::NativeState {
            codec: EngineCodec::LibghosttyCheckpointV2,
        },
        cols: 120,
        rows: 40,
        base_seq: 99,
    });
    round_trip(FrameKind::BootstrapChunk {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        chunk_seq: 0,
        payload: Bytes::from_static(b"opaque-checkpoint-records"),
    });
    round_trip(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: Some(Bytes::from_static(b"engine-cursor-1")),
    });
    round_trip(FrameKind::HistoryRequest {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        cursor: Bytes::from_static(b"engine-cursor-1"),
        max_bytes: 64 * 1024,
        max_rows: 1024,
    });
    round_trip(FrameKind::HistoryPage {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        page_seq: 1,
        cursor: Bytes::from_static(b"engine-cursor-1"),
        next_cursor: Some(Bytes::from_static(b"engine-cursor-2")),
        payload: Bytes::from_static(b"opaque-history-page"),
        rows: 512,
    });
    round_trip(FrameKind::HistoryPage {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        page_seq: 1,
        cursor: Bytes::from_static(b"engine-cursor-2"),
        next_cursor: None,
        payload: Bytes::from_static(b"opaque-finish-record"),
        rows: 0,
    });
    round_trip(FrameKind::BootstrapTombstone {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        reason: TombstoneReason::OutboundGap,
        last_valid_seq: 123,
    });
    round_trip(FrameKind::HistoryTombstone {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        cursor: Bytes::from_static(b"stale-cursor"),
        reason: HistoryTombstoneReason::Pruned,
    });
    round_trip(FrameKind::HistoryRejected {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        cursor: Bytes::from_static(b"retry-cursor"),
        reason: HistoryRejectionReason::TooSmall,
        required_bytes: 8192,
        required_rows: 256,
    });
    round_trip(FrameKind::AttachReady { attach_id: 17 });
    round_trip(FrameKind::TerminalOutput {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        seq: 100,
        bytes: Bytes::from_static(b"\x1b[38;2;1;2;3mRAW\x1b[0m"),
    });
    round_trip(FrameKind::FrameAck {
        terminal_id,
        stream_id,
        bootstrap_id,
        seq: 100,
    });
}

#[test]
fn hello_and_all_three_selected_profiles_round_trip() {
    round_trip(FrameKind::Hello {
        client_name: "phux-native-test".to_owned(),
        protocol_major: 0,
        protocol_minor: 7,
        protocol_patch: 0,
        client_caps: ClientCapabilities::new(),
    });

    for selected_profile in [
        BootstrapProfile::NativeState {
            codec: EngineCodec::LibghosttyCheckpointV2,
            features: EngineFeatureSet::required_native(),
        },
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapProfile::SynthesizedVtStateSync,
    ] {
        round_trip(FrameKind::HelloOk {
            protocol_major: 0,
            protocol_minor: 7,
            protocol_patch: 0,
            server_caps: ServerCapabilities::new(),
            server_id: b"server-incarnation".to_vec(),
            selected_profile,
            bootstrap_limits: BootstrapLimits::new(128 * 1024, 512 * 1024).unwrap(),
        });
    }
}

#[test]
fn hello_rejects_each_omitted_required_field() {
    let hello = FrameKind::Hello {
        client_name: "required-fields".to_owned(),
        protocol_major: 0,
        protocol_minor: 7,
        protocol_patch: 0,
        client_caps: ClientCapabilities::new(),
    };
    for omitted_id in 1..=5 {
        assert_eq!(
            FrameKind::decode(&encode_without_field(&hello, omitted_id)).unwrap_err(),
            DecodeError::UnexpectedEof,
            "HELLO field {omitted_id} must be required",
        );
    }
}

#[test]
fn hello_ok_rejects_each_omitted_required_field() {
    let hello_ok = FrameKind::HelloOk {
        protocol_major: 0,
        protocol_minor: 7,
        protocol_patch: 0,
        server_caps: ServerCapabilities::new(),
        server_id: Vec::new(),
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    };
    for omitted_id in 1..=8 {
        assert_eq!(
            FrameKind::decode(&encode_without_field(&hello_ok, omitted_id)).unwrap_err(),
            DecodeError::UnexpectedEof,
            "HELLO_OK field {omitted_id} must be required",
        );
    }
}

#[test]
fn every_new_frame_encodes_fields_in_allocated_order() {
    let terminal_id = TerminalId::local(1);
    let stream_id = stream(2);
    let bootstrap_id = bootstrap(3);
    let cases = [
        (
            FrameKind::HelloOk {
                protocol_major: 0,
                protocol_minor: 7,
                protocol_patch: 0,
                server_caps: ServerCapabilities::new(),
                server_id: b"incarnation".to_vec(),
                selected_profile: BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: BootstrapLimits::default(),
            },
            vec![1, 2, 3, 4, 5, 6, 7, 8],
        ),
        (FrameKind::AttachReady { attach_id: 4 }, vec![1]),
        (
            FrameKind::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 80,
                rows: 24,
                base_seq: 5,
            },
            vec![1, 2, 3, 4, 5, 6, 7, 8],
        ),
        (
            FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: Bytes::from_static(b"chunk"),
            },
            vec![1, 2, 3, 4, 5],
        ),
        (
            FrameKind::BootstrapReady {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                history_cursor: Some(Bytes::from_static(b"cursor")),
            },
            vec![1, 2, 3, 4],
        ),
        (
            FrameKind::HistoryRequest {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                cursor: Bytes::from_static(b"cursor"),
                max_bytes: 4096,
                max_rows: 1024,
            },
            vec![1, 2, 3, 4, 5, 6],
        ),
        (
            FrameKind::HistoryPage {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                page_seq: 1,
                cursor: Bytes::from_static(b"cursor"),
                next_cursor: Some(Bytes::from_static(b"next")),
                payload: Bytes::from_static(b"page"),
                rows: 4,
            },
            vec![1, 2, 3, 4, 5, 6, 7, 8],
        ),
        (
            FrameKind::BootstrapTombstone {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                reason: TombstoneReason::Resize,
                last_valid_seq: 5,
            },
            vec![1, 2, 3, 4, 5],
        ),
        (
            FrameKind::HistoryTombstone {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                cursor: Bytes::from_static(b"cursor"),
                reason: HistoryTombstoneReason::Expired,
            },
            vec![1, 2, 3, 4, 5],
        ),
        (
            FrameKind::HistoryRejected {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                cursor: Bytes::from_static(b"cursor"),
                reason: HistoryRejectionReason::Busy,
                required_bytes: 4096,
                required_rows: 256,
            },
            vec![1, 2, 3, 4, 5, 6, 7],
        ),
        (
            FrameKind::TerminalOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq: 6,
                bytes: Bytes::from_static(b"live"),
            },
            vec![1, 2, 3, 4, 5],
        ),
    ];
    for (frame, expected) in cases {
        assert_eq!(encoded_field_ids(&frame), expected);
    }
}

#[test]
fn unknown_top_level_fields_are_skipped_without_touching_opaque_bytes() {
    let frame = FrameKind::BootstrapChunk {
        terminal_id: TerminalId::local(1),
        stream_id: stream(2),
        bootstrap_id: bootstrap(3),
        chunk_seq: 4,
        payload: Bytes::from_static(b"\x00\xffengine-owned\x1b[31m"),
    };
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    let mut unknown = Vec::new();
    tlv_field(&mut unknown, 99, b"future-field");
    encoded.extend_from_slice(&unknown);
    let new_length = u32::from_be_bytes(encoded[..4].try_into().unwrap())
        .checked_add(u32::try_from(unknown.len()).unwrap())
        .unwrap();
    encoded[..4].copy_from_slice(&new_length.to_be_bytes());

    let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());
}

#[test]
fn zero_stream_and_bootstrap_ids_are_rejected_before_dispatch() {
    let terminal = local_terminal(1);
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &terminal);
    tlv_field(&mut fields, 2, &0u64.to_be_bytes());
    tlv_field(&mut fields, 3, &1u64.to_be_bytes());
    assert_eq!(
        FrameKind::decode(&framed(TYPE_BOOTSTRAP_READY, &fields)).unwrap_err(),
        DecodeError::InvalidStreamId
    );

    fields.clear();
    tlv_field(&mut fields, 1, &terminal);
    tlv_field(&mut fields, 2, &1u64.to_be_bytes());
    tlv_field(&mut fields, 3, &0u64.to_be_bytes());
    assert_eq!(
        FrameKind::decode(&framed(TYPE_BOOTSTRAP_READY, &fields)).unwrap_err(),
        DecodeError::InvalidBootstrapId
    );
}

#[test]
fn history_page_sequence_and_row_count_are_required() {
    let page = FrameKind::HistoryPage {
        terminal_id: TerminalId::local(1),
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
        terminal_id: TerminalId::local(1),
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
    round_trip(FrameKind::HistoryRequest {
        terminal_id: TerminalId::local(1),
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
        round_trip(FrameKind::HistoryTombstone {
            terminal_id: TerminalId::local(1),
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
        round_trip(FrameKind::HistoryRejected {
            terminal_id: TerminalId::local(1),
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
        terminal_id: TerminalId::local(1),
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
        terminal_id: TerminalId::local(1),
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
            terminal_id: TerminalId::local(1),
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
        terminal_id: TerminalId::local(1),
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
        terminal_id: TerminalId::local(1),
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
        terminal_id: TerminalId::local(1),
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
        terminal_id: TerminalId::local(1),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        cursor: Bytes::from_static(b"cursor"),
        max_bytes: MAX_HISTORY_PAGE_BYTES + 1,
        max_rows: 1024,
    };
    round_trip(over_bytes);

    let over_rows = FrameKind::HistoryRequest {
        terminal_id: TerminalId::local(1),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        cursor: Bytes::from_static(b"cursor"),
        max_bytes: 4096,
        max_rows: MAX_HISTORY_PAGE_ROWS + 1,
    };
    round_trip(over_rows);

    let over_page_rows = FrameKind::HistoryPage {
        terminal_id: TerminalId::local(1),
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

#[test]
fn profile_selection_prefers_native_then_explicit_compatibility() {
    let server = BootstrapCapabilities::new()
        .with_native(
            EngineCodec::LibghosttyCheckpointV2,
            EngineFeatureSet::required_native(),
        )
        .with_limits(BootstrapLimits::new(64 * 1024, 256 * 1024).unwrap());
    let client = ClientCapabilities::new().with_bootstrap(
        BootstrapCapabilities::new()
            .with_native(
                EngineCodec::LibghosttyCheckpointV2,
                EngineFeatureSet::required_native(),
            )
            .with_limits(BootstrapLimits::new(128 * 1024, 128 * 1024).unwrap()),
    );
    let (profile, limits) = select_bootstrap_profile(&client, &server).unwrap();
    assert_eq!(
        profile,
        BootstrapProfile::NativeState {
            codec: EngineCodec::LibghosttyCheckpointV2,

            features: EngineFeatureSet::required_native(),
        }
    );
    assert_eq!(limits.max_chunk_bytes(), 64 * 1024);
    assert_eq!(limits.max_history_page_bytes(), 128 * 1024);

    let state_sync_only =
        BootstrapProfileSet::with(&[BootstrapProfileKind::SynthesizedVtStateSync]);
    let client = ClientCapabilities::new()
        .with_output_mode(OutputMode::StateSync)
        .with_bootstrap(BootstrapCapabilities::new().with_profiles(state_sync_only));
    let server = BootstrapCapabilities::new().with_profiles(state_sync_only);
    assert_eq!(
        select_bootstrap_profile(&client, &server).unwrap().0,
        BootstrapProfile::SynthesizedVtStateSync
    );

    let native_only = BootstrapProfileSet::with(&[BootstrapProfileKind::NativeState]);
    let client = ClientCapabilities::new().with_bootstrap(
        BootstrapCapabilities::new()
            .with_native(
                EngineCodec::LibghosttyCheckpointV2,
                EngineFeatureSet::required_native(),
            )
            .with_profiles(native_only)
            .with_native_codecs(EngineCodecSet::new()),
    );
    let server = BootstrapCapabilities::new()
        .with_native(
            EngineCodec::LibghosttyCheckpointV2,
            EngineFeatureSet::required_native(),
        )
        .with_profiles(native_only);
    assert!(select_bootstrap_profile(&client, &server).is_err());
}

#[test]
fn native_selection_uses_the_highest_exact_known_common_codec() {
    let future_codec_bit = 1_u64 << 63;
    let v2_bit = 1_u64 << EngineCodec::LibghosttyCheckpointV2.as_wire();
    let client_codecs = EngineCodecSet::from_wire(future_codec_bit | v2_bit);
    let server_codecs = EngineCodecSet::from_wire(v2_bit);
    assert_eq!(
        client_codecs.highest_common(server_codecs),
        Some(EngineCodec::LibghosttyCheckpointV2)
    );

    let native_only = BootstrapProfileSet::with(&[BootstrapProfileKind::NativeState]);
    let client = ClientCapabilities::new().with_bootstrap(
        BootstrapCapabilities::new()
            .with_profiles(native_only)
            .with_native_codecs(client_codecs)
            .with_native_features(EngineFeatureSet::required_native()),
    );
    let server = BootstrapCapabilities::new()
        .with_profiles(native_only)
        .with_native_codecs(server_codecs)
        .with_native_features(EngineFeatureSet::required_native());
    assert_eq!(
        select_bootstrap_profile(&client, &server).unwrap().0,
        BootstrapProfile::NativeState {
            codec: EngineCodec::LibghosttyCheckpointV2,
            features: EngineFeatureSet::required_native(),
        }
    );
}

#[test]
fn future_only_codec_bits_never_select_native() {
    let future_only = EngineCodecSet::from_wire(1_u64 << 63);
    assert_eq!(future_only, EngineCodecSet::new());

    let advertised = BootstrapProfileSet::all();
    let client = ClientCapabilities::new().with_bootstrap(
        BootstrapCapabilities::new()
            .with_profiles(advertised)
            .with_native_codecs(future_only)
            .with_native_features(EngineFeatureSet::required_native()),
    );
    let server = BootstrapCapabilities::new()
        .with_profiles(advertised)
        .with_native_codecs(future_only)
        .with_native_features(EngineFeatureSet::required_native());
    assert_eq!(
        select_bootstrap_profile(&client, &server).unwrap().0,
        BootstrapProfile::SynthesizedVtRaw
    );

    let native_only = BootstrapProfileSet::with(&[BootstrapProfileKind::NativeState]);
    let client =
        ClientCapabilities::new().with_bootstrap(client.bootstrap.with_profiles(native_only));
    let server = server.with_profiles(native_only);
    assert!(select_bootstrap_profile(&client, &server).is_err());
}

#[test]
fn every_native_required_feature_is_strict() {
    let incomplete_feature_sets = [
        EngineFeatureSet::with(&[
            EngineFeature::ReadyBoundary,
            EngineFeature::HistoryPages,
            EngineFeature::BoundedHistoryControl,
        ]),
        EngineFeatureSet::with(&[
            EngineFeature::Continuation,
            EngineFeature::HistoryPages,
            EngineFeature::BoundedHistoryControl,
        ]),
        EngineFeatureSet::with(&[
            EngineFeature::Continuation,
            EngineFeature::ReadyBoundary,
            EngineFeature::BoundedHistoryControl,
        ]),
        EngineFeatureSet::with(&[
            EngineFeature::Continuation,
            EngineFeature::ReadyBoundary,
            EngineFeature::HistoryPages,
        ]),
    ];
    let native_and_raw = BootstrapProfileSet::with(&[
        BootstrapProfileKind::NativeState,
        BootstrapProfileKind::SynthesizedVtRaw,
    ]);
    let native_only = BootstrapProfileSet::with(&[BootstrapProfileKind::NativeState]);
    let codecs = EngineCodecSet::with(&[EngineCodec::LibghosttyCheckpointV2]);
    let server = BootstrapCapabilities::new()
        .with_profiles(native_and_raw)
        .with_native_codecs(codecs)
        .with_native_features(EngineFeatureSet::required_native());

    for features in incomplete_feature_sets {
        let bootstrap = BootstrapCapabilities::new()
            .with_profiles(native_and_raw)
            .with_native_codecs(codecs)
            .with_native_features(features);
        let client = ClientCapabilities::new().with_bootstrap(bootstrap);
        assert_eq!(
            select_bootstrap_profile(&client, &server).unwrap().0,
            BootstrapProfile::SynthesizedVtRaw,
            "partial native features must fall back explicitly"
        );

        let native_required =
            ClientCapabilities::new().with_bootstrap(bootstrap.with_profiles(native_only));
        assert!(
            select_bootstrap_profile(&native_required, &server.with_profiles(native_only)).is_err(),
            "partial native features must reject when no synthesis profile is common"
        );
    }
}

#[test]
fn versioned_native_offer_forces_mixed_peers_to_synthesis_or_rejection() {
    const LEGACY_NATIVE_BIT: u8 = 0x01;
    const SYNTHESIZED_RAW_BIT: u8 = 0x02;
    const LEGACY_PROFILE_MASK: u8 = 0x07;

    let new_server = BootstrapCapabilities::new().with_native(
        EngineCodec::LibghosttyCheckpointV2,
        EngineFeatureSet::required_native(),
    );
    let new_offer_as_seen_by_old_server = new_server.profiles.as_wire() & LEGACY_PROFILE_MASK;
    let old_server_selection = if new_offer_as_seen_by_old_server & LEGACY_NATIVE_BIT != 0 {
        LEGACY_NATIVE_BIT
    } else if new_offer_as_seen_by_old_server & SYNTHESIZED_RAW_BIT != 0 {
        SYNTHESIZED_RAW_BIT
    } else {
        0
    };
    assert_eq!(
        old_server_selection, SYNTHESIZED_RAW_BIT,
        "an old server must not mistake the versioned native offer for legacy native"
    );

    let mut legacy_hello_ok = Vec::new();
    tlv_field(&mut legacy_hello_ok, 1, &0_u16.to_be_bytes());
    tlv_field(&mut legacy_hello_ok, 2, &7_u16.to_be_bytes());
    tlv_field(&mut legacy_hello_ok, 3, &0_u16.to_be_bytes());
    tlv_field(&mut legacy_hello_ok, 4, &[1, 0, 0, 0, 0]);
    tlv_field(&mut legacy_hello_ok, 5, b"legacy-server");
    tlv_field(
        &mut legacy_hello_ok,
        6,
        &[0, EngineCodec::LibghosttyCheckpointV2.as_wire(), 0, 0, 0, 7],
    );
    assert_eq!(
        FrameKind::decode(&framed(TYPE_HELLO_OK, &legacy_hello_ok)).unwrap_err(),
        DecodeError::UnknownEnumValue {
            field: "BootstrapProfile",
            value: 0,
        },
        "a new client must never accept the retired selected-profile tag"
    );

    let old_client_profiles =
        BootstrapProfileSet::from_wire(LEGACY_NATIVE_BIT | SYNTHESIZED_RAW_BIT);
    let old_client = ClientCapabilities::new()
        .with_bootstrap(BootstrapCapabilities::new().with_profiles(old_client_profiles));
    assert_eq!(
        select_bootstrap_profile(&old_client, &new_server)
            .unwrap()
            .0,
        BootstrapProfile::SynthesizedVtRaw,
        "a new server must ignore the retired legacy native bit"
    );

    let old_native_only = ClientCapabilities::new().with_bootstrap(
        BootstrapCapabilities::new()
            .with_profiles(BootstrapProfileSet::from_wire(LEGACY_NATIVE_BIT)),
    );
    assert!(
        select_bootstrap_profile(&old_native_only, &new_server).is_err(),
        "legacy native without a shared synthesized profile must fail before attach"
    );
}

#[test]
fn old_engine_capabilities_fall_back_without_changing_protocol_version() {
    let old_engine_client = ClientCapabilities::new();
    let native_server = BootstrapCapabilities::new().with_native(
        EngineCodec::LibghosttyCheckpointV2,
        EngineFeatureSet::required_native(),
    );
    assert_eq!(
        select_bootstrap_profile(&old_engine_client, &native_server)
            .unwrap()
            .0,
        BootstrapProfile::SynthesizedVtRaw
    );
    assert_eq!((PROTOCOL_VERSION.major, PROTOCOL_VERSION.minor), (0, 7));
    assert_eq!(EngineCodec::LibghosttyCheckpointV2.as_wire(), 2);
}

#[test]
fn opaque_lifecycle_records_reencode_byte_identically() {
    let terminal_id = TerminalId::local(44);
    let stream_id = stream(45);
    let bootstrap_id = bootstrap(46);
    let future_record = Bytes::from_static(b"\xff\x80\0future-checkpoint-v255\xfe");
    let cursor = Bytes::from_static(b"\0\xffcursor\x80");
    let next_cursor = Bytes::from_static(b"\xfe\xfdnext\0");
    let frames = [
        FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::NativeState {
                codec: EngineCodec::LibghosttyCheckpointV2,
            },
            cols: 132,
            rows: 43,
            base_seq: 901,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: future_record.clone(),
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: Some(cursor.clone()),
        },
        FrameKind::HistoryRequest {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: cursor.clone(),
            max_bytes: 4096,
            max_rows: 1024,
        },
        FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            page_seq: 1,
            cursor,
            next_cursor: Some(next_cursor),
            payload: future_record,
            rows: 3,
        },
    ];

    for frame in frames {
        let mut encoded = BytesMut::new();
        frame.encode(&mut encoded);
        let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
        assert!(tail.is_empty());
        assert_eq!(decoded, frame);

        let mut reencoded = BytesMut::new();
        decoded.encode(&mut reencoded);
        assert_eq!(reencoded, encoded);
    }
}

#[test]
fn negotiated_response_payload_limits_reject_but_request_budgets_reach_the_host() {
    let limits = BootstrapLimits::new(1024, 2048).unwrap();
    let chunk = FrameKind::BootstrapChunk {
        terminal_id: TerminalId::local(1),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        chunk_seq: 0,
        payload: Bytes::from(vec![0; 1025]),
    };
    let mut encoded = BytesMut::new();
    chunk.encode(&mut encoded);
    assert_eq!(
        FrameKind::decode_with_limits(&encoded, limits).unwrap_err(),
        DecodeError::BootstrapLimitExceeded
    );
    assert!(FrameKind::decode(&encoded).is_ok());

    let page = FrameKind::HistoryPage {
        terminal_id: TerminalId::local(1),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        page_seq: 1,
        cursor: Bytes::from_static(b"cursor"),
        next_cursor: None,
        payload: Bytes::from(vec![0; 2049]),
        rows: 1,
    };
    encoded.clear();
    page.encode(&mut encoded);
    assert_eq!(
        FrameKind::decode_with_limits(&encoded, limits).unwrap_err(),
        DecodeError::BootstrapLimitExceeded
    );
    assert!(FrameKind::decode(&encoded).is_ok());

    let request = FrameKind::HistoryRequest {
        terminal_id: TerminalId::local(1),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        cursor: Bytes::from_static(b"cursor"),
        max_bytes: 2049,
        max_rows: 1024,
    };
    encoded.clear();
    request.encode(&mut encoded);
    let (decoded, tail) = FrameKind::decode_with_limits(&encoded, limits).unwrap();
    assert!(tail.is_empty());
    assert_eq!(decoded, request);
}

#[test]
fn native_live_bytes_are_codec_opaque_and_round_trip_exactly() {
    let raw = Bytes::from_static(b"\x1b]8;;https://example.invalid\x07link\x1b]8;;\x07\xff");
    let frame = FrameKind::TerminalOutput {
        terminal_id: TerminalId::local(8),
        stream_id: stream(8),
        bootstrap_id: bootstrap(8),
        seq: 1,
        bytes: raw.clone(),
    };
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    let (decoded, _) = FrameKind::decode(&encoded).unwrap();
    let FrameKind::TerminalOutput { bytes, .. } = decoded else {
        panic!("expected terminal output");
    };
    assert_eq!(bytes, raw);
}
