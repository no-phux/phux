//! Focused protocol-0.7 bootstrap/profile wire contract tests.

#![allow(clippy::unwrap_used)]

use bytes::{Bytes, BytesMut};
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfile, BootstrapProfileKind,
    BootstrapProfileSet, BootstrapStreamProfile, ClientCapabilities, EngineCodec, EngineCodecSet,
    EngineFeatureSet, MAX_BOOTSTRAP_CHUNK_BYTES, MAX_HISTORY_PAGE_BYTES, OutputMode,
    ServerCapabilities, select_bootstrap_profile,
};
use phux_protocol::ids::{BootstrapId, StreamId, TerminalId};
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::{
    FrameKind, TombstoneReason, MAX_HISTORY_CURSOR_BYTES, TYPE_ATTACH_READY,
    TYPE_BOOTSTRAP_BEGIN, TYPE_BOOTSTRAP_CHUNK, TYPE_BOOTSTRAP_READY,
    TYPE_BOOTSTRAP_TOMBSTONE, TYPE_HISTORY_PAGE, TYPE_HISTORY_REQUEST,
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
    });
    round_trip(FrameKind::HistoryPage {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        cursor: Bytes::from_static(b"engine-cursor-1"),
        next_cursor: Some(Bytes::from_static(b"engine-cursor-2")),
        payload: Bytes::from_static(b"opaque-history-page"),
    });
    round_trip(FrameKind::HistoryPage {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        cursor: Bytes::from_static(b"engine-cursor-2"),
        next_cursor: None,
        payload: Bytes::from_static(b"opaque-finish-record"),
    });
    round_trip(FrameKind::BootstrapTombstone {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        reason: TombstoneReason::OutboundGap,
        last_valid_seq: 123,
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
            },
            vec![1, 2, 3, 4, 5],
        ),
        (
            FrameKind::HistoryPage {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                cursor: Bytes::from_static(b"cursor"),
                next_cursor: Some(Bytes::from_static(b"next")),
                payload: Bytes::from_static(b"page"),
            },
            vec![1, 2, 3, 4, 5, 6],
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
fn hard_payload_and_request_bounds_are_enforced() {
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
        cursor: Bytes::from_static(b"cursor"),
        next_cursor: None,
        payload: Bytes::from(vec![0; MAX_HISTORY_PAGE_BYTES as usize + 1]),
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

    for invalid in [0, MAX_HISTORY_PAGE_BYTES + 1] {
        let request = FrameKind::HistoryRequest {
            terminal_id: TerminalId::local(1),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            cursor: Bytes::from_static(b"cursor"),
            max_bytes: invalid,
        };
        encoded.clear();
        request.encode(&mut encoded);
        assert_eq!(
            FrameKind::decode(&encoded).unwrap_err(),
            DecodeError::BootstrapLimitExceeded
        );
    }
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
    let server = BootstrapCapabilities::new().with_limits(
        BootstrapLimits::new(64 * 1024, 256 * 1024).unwrap(),
    );
    let client = ClientCapabilities::new().with_bootstrap(
        BootstrapCapabilities::new().with_limits(
            BootstrapLimits::new(128 * 1024, 128 * 1024).unwrap(),
        ),
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

    let state_sync_only = BootstrapProfileSet::with(&[
        BootstrapProfileKind::SynthesizedVtStateSync,
    ]);
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
            .with_profiles(native_only)
            .with_native_codecs(EngineCodecSet::new()),
    );
    let server = BootstrapCapabilities::new().with_profiles(native_only);
    assert!(select_bootstrap_profile(&client, &server).is_err());
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
