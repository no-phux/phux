//! Bootstrap frame discriminants, required fields, field order, and id validation.

use super::{
    BootstrapLimits, BootstrapProfile, BootstrapProfileKind, BootstrapProfileSet,
    BootstrapStreamProfile, Bytes, ClientCapabilities, DecodeError, EngineCodec, EngineFeatureSet,
    FrameKind, HistoryRejectionReason, HistoryTombstoneReason, ResourceId, ServerCapabilities,
    TYPE_ATTACH_READY, TYPE_BOOTSTRAP_BEGIN, TYPE_BOOTSTRAP_CHUNK, TYPE_BOOTSTRAP_READY,
    TYPE_BOOTSTRAP_TOMBSTONE, TYPE_HISTORY_PAGE, TYPE_HISTORY_REJECTED, TYPE_HISTORY_REQUEST,
    TYPE_HISTORY_TOMBSTONE, TombstoneReason, assert_round_trip, bootstrap, chunk,
    encode_without_field, encoded_field_ids, framed, history_page, history_rejected,
    history_request, history_tombstone, local_terminal, stream, tlv_field,
};

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

/// BEGIN/CHUNK/READY/REQUEST/PAGE, `RESOURCE_OUTPUT`, and `FRAME_ACK` are drawn
/// by the `wire_roundtrip` proptests; history status frames by the enum
/// table below.
#[test]
fn bootstrap_tombstone_and_attach_ready_round_trip() {
    assert_round_trip(&FrameKind::BootstrapTombstone {
        terminal_id: ResourceId::local(42),
        stream_id: stream(7),
        bootstrap_id: bootstrap(9),
        reason: TombstoneReason::OutboundGap,
        last_valid_seq: 123,
    });
    assert_round_trip(&FrameKind::AttachReady { attach_id: 17 });
}

/// A protocol-0.7 `HELLO_OK`.
fn hello_ok(
    server_id: &[u8],
    selected_profile: Option<BootstrapProfile>,
    bootstrap_limits: BootstrapLimits,
) -> FrameKind {
    FrameKind::HelloOk {
        protocol_major: 0,
        protocol_minor: 7,
        protocol_patch: 0,
        server_caps: ServerCapabilities::new(),
        server_id: server_id.to_vec(),
        selected_profile,
        bootstrap_limits,
    }
}

#[test]
fn hello_and_all_three_selected_profiles_round_trip() {
    assert_round_trip(&FrameKind::Hello {
        client_name: "phux-native-test".to_owned(),
        protocol_major: 0,
        protocol_minor: 7,
        protocol_patch: 0,
        client_caps: ClientCapabilities::new(),
    });

    // `None` is no Terminal profile: the field is absent, and an absent
    // field decodes as `None`.
    for selected_profile in [
        Some(BootstrapProfile::NativeState {
            codec: EngineCodec::LibghosttyCheckpointV2,
            features: EngineFeatureSet::required_native(),
        }),
        Some(BootstrapProfile::SynthesizedVtRaw),
        Some(BootstrapProfile::SynthesizedVtStateSync),
        None,
    ] {
        assert_round_trip(&hello_ok(
            b"server-incarnation",
            selected_profile,
            BootstrapLimits::new(128 * 1024, 512 * 1024).unwrap(),
        ));
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
    let hello_ok = hello_ok(
        b"",
        Some(BootstrapProfile::SynthesizedVtRaw),
        BootstrapLimits::default(),
    );
    // Field 6, `SELECTED_PROFILE`, is optional: absent means no Terminal
    // profile was selected.
    for omitted_id in (1..=8).filter(|id| *id != 6) {
        assert_eq!(
            FrameKind::decode(&encode_without_field(&hello_ok, omitted_id)).unwrap_err(),
            DecodeError::UnexpectedEof,
            "HELLO_OK field {omitted_id} must be required",
        );
    }
    assert!(matches!(
        FrameKind::decode(&encode_without_field(&hello_ok, 6)),
        Ok((
            FrameKind::HelloOk {
                selected_profile: None,
                ..
            },
            _
        ))
    ));
}

#[allow(clippy::too_many_lines)]
#[test]
fn every_new_frame_encodes_fields_in_allocated_order() {
    let terminal_id = ResourceId::local(1);
    let stream_id = stream(2);
    let bootstrap_id = bootstrap(3);
    let mut page_with_next_cursor = history_page(Bytes::from_static(b"page"), 4);
    if let FrameKind::HistoryPage { next_cursor, .. } = &mut page_with_next_cursor {
        *next_cursor = Some(Bytes::from_static(b"next"));
    }
    let cases = [
        (
            hello_ok(
                b"incarnation",
                Some(BootstrapProfile::SynthesizedVtRaw),
                BootstrapLimits::default(),
            ),
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
        (chunk(Bytes::from_static(b"chunk")), vec![1, 2, 3, 4, 5]),
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
            history_request(b"cursor", 4096, 1024),
            vec![1, 2, 3, 4, 5, 6],
        ),
        (page_with_next_cursor, vec![1, 2, 3, 4, 5, 6, 7, 8]),
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
            history_tombstone(HistoryTombstoneReason::Expired),
            vec![1, 2, 3, 4, 5],
        ),
        (
            history_rejected(HistoryRejectionReason::Busy, 4096, 256),
            vec![1, 2, 3, 4, 5, 6, 7],
        ),
        (
            FrameKind::ResourceOutput {
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
        assert_eq!(encoded_field_ids(&frame), expected, "{frame:?}");
    }
}

#[test]
fn unknown_top_level_fields_are_skipped_without_touching_opaque_bytes() {
    let mut frame = chunk(Bytes::from_static(b"\x00\xffengine-owned\x1b[31m"));
    if let FrameKind::BootstrapChunk { chunk_seq, .. } = &mut frame {
        *chunk_seq = 4;
    }
    let encoded = crate::common::encode_with_unknown_field(&frame, 99, b"future-field");
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
