//! Frame round-trip properties, handshake shapes, and malformed-input rejection.

use proptest::prelude::*;

use super::{
    BootstrapId, BootstrapStreamProfile, BytesMut, ClientCapabilities, ClientId, ColorSupport,
    DecodeError, Decoder, EngineCodec, ErrorCode, FrameKind, ImageProtocol, ImageProtocolSet,
    KeyboardProtocol, KeyboardProtocolSet, Layer, LayerSet, OutputMode, ResourceId, ResourceInfo,
    SessionId, SessionSnapshot, StreamId, TerminalColor, TerminalDefaultColors, WindowId,
    arb_frame_kind, arb_resource_info, arb_terminal_id, assert_round_trip, common, framed_tlv,
    tlv_field,
};

proptest! {
    // Five catalog groups share this budget; 2048 cases keeps per-group
    // draws in the same order as the deleted thin wrappers.
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// Encoding then decoding any catalog `FrameKind` is the identity.
    #[test]
    fn roundtrip_frame_kind(frame in arb_frame_kind()) {
        assert_round_trip(&frame);
    }
}

proptest! {
    /// Decoding never panics on arbitrary byte input. The result is either
    /// a successful parse (rare but possible by luck) or a `DecodeError`.
    #[test]
    fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        let _ = Decoder::new(&bytes).read_frame();
    }

    /// Mutating a valid encoding of any catalog frame never panics the
    /// decoder. Random bytes rarely get past the header, so this reaches
    /// every body decoder with input one edit away from well formed. The
    /// header is re-stamped after the edits so the body is what is fuzzed.
    #[test]
    fn mutated_catalog_frames_never_panic(
        frame in arb_frame_kind(),
        edits in proptest::collection::vec((any::<usize>(), any::<u8>(), 0_u8..4), 1..6),
    ) {
        let mut bytes = common::encode(&frame);
        for (at, byte, op) in edits {
            if bytes.len() <= 5 {
                break;
            }
            let at = 5 + at % (bytes.len() - 5);
            match op {
                0 => bytes[at] = byte,
                1 => bytes[at] ^= 1 << (byte % 8),
                2 => bytes.insert(at, byte),
                _ => bytes.truncate(at),
            }
        }
        let body_len = u32::try_from(bytes.len() - 4).unwrap();
        bytes[..4].copy_from_slice(&body_len.to_be_bytes());
        let _ = Decoder::new(&bytes).read_frame();
    }

    #[test]
    fn opaque_bootstrap_lifecycle_round_trips_arbitrary_bytes(
        terminal_id in arb_terminal_id(),
        stream_raw in 1_u64..=u64::MAX,
        bootstrap_raw in 1_u64..=u64::MAX,
        cols in 1_u16..=u16::MAX,
        rows in 1_u16..=u16::MAX,
        base_seq in any::<u64>(),
        chunk_seq in any::<u32>(),
        page_seq in 1_u64..=u64::MAX,
        history_rows in 0_u32..=4096,
        checkpoint in proptest::collection::vec(any::<u8>(), 0..4096),
        cursor in proptest::collection::vec(any::<u8>(), 0..4096),
        next_cursor in proptest::option::of(proptest::collection::vec(any::<u8>(), 0..4096)),
        history in proptest::collection::vec(any::<u8>(), 0..4096),
    ) {
        let stream_id = StreamId::new(stream_raw).unwrap();
        let bootstrap_id = BootstrapId::new(bootstrap_raw).unwrap();
        let frames = [
            FrameKind::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::NativeState {
                    codec: EngineCodec::LibghosttyCheckpointV2,
                },
                cols,
                rows,
                base_seq,
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload: checkpoint.into(),
            },
            FrameKind::BootstrapReady {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                history_cursor: Some(cursor.clone().into()),
            },
            FrameKind::HistoryRequest {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                cursor: cursor.clone().into(),
                max_bytes: 4096,
                max_rows: 4096,
            },
            FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                cursor: cursor.into(),
                next_cursor: next_cursor.map(Into::into),
                payload: history.into(),
                rows: history_rows,
            },
        ];

        for frame in frames {
            assert_round_trip(&frame);
        }
    }
}

/// HELLO round-trips across every capability shape a client can advertise:
/// the default caps, each `ColorSupport` variant, image/kbd/hyperlink
/// combinations, `OutputMode::StateSync` (phux-fseo), and outer terminal
/// default colors.
#[test]
fn hello_round_trips_across_capability_shapes() {
    let mut shapes = vec![
        ClientCapabilities::default(),
        ClientCapabilities::new()
            .with_image_protocols(ImageProtocolSet::with(&[ImageProtocol::Sixel]))
            .with_kbd_protocols(KeyboardProtocolSet::with(&[KeyboardProtocol::Kitty]))
            .with_hyperlinks(false),
        ClientCapabilities::new().with_output_mode(OutputMode::StateSync),
        ClientCapabilities::new().with_quic_streams(true),
        ClientCapabilities::new().with_default_colors(TerminalDefaultColors {
            foreground: TerminalColor {
                r: 0xd0,
                g: 0xd0,
                b: 0xd0,
            },
            background: TerminalColor {
                r: 0x12,
                g: 0x18,
                b: 0x1b,
            },
        }),
    ];
    for color in [
        ColorSupport::TrueColor,
        ColorSupport::Indexed256,
        ColorSupport::Indexed16,
        ColorSupport::Mono,
    ] {
        shapes.push(ClientCapabilities::new().with_color_support(color));
    }
    // HELLO field 9, the origin `phux stdio-bridge` stamps: both address
    // families, with and without the server endpoint.
    for (client, server) in [
        ("203.0.113.5:52144", Some("198.51.100.7:22")),
        ("[2001:db8::1]:40000", None),
    ] {
        shapes.push(ClientCapabilities::new().with_ssh_origin(
            phux_protocol::wire::ssh_origin::SshOrigin {
                client: client.parse().expect("client endpoint"),
                server: server.map(|server| server.parse().expect("server endpoint")),
            },
        ));
    }
    for client_caps in shapes {
        assert_round_trip(&FrameKind::Hello {
            client_name: "phux-client".to_owned(),
            protocol_major: 0,
            protocol_minor: 2,
            protocol_patch: 0,
            client_caps,
        });
    }
}

/// Protocol 0.7 requires an explicit selected profile and negotiated bounds.
#[test]
fn hello_ok_without_profile_is_rejected() {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &0u16.to_be_bytes());
    tlv_field(&mut fields, 2, &7u16.to_be_bytes());
    tlv_field(&mut fields, 3, &0u16.to_be_bytes());
    let framed = framed_tlv(0x80, &fields);
    assert_eq!(
        FrameKind::decode(&framed).unwrap_err(),
        DecodeError::UnexpectedEof
    );
}

/// Outer-frame rejections: truncated header, zero or over-cap length, a
/// length past the buffer, a truncated body, and unknown or retired type
/// bytes (0x40 is the pre-ADR-0013 `PANE_DIFF`).
#[test]
fn malformed_outer_frames_are_rejected() {
    let frame = |len: u32, rest: &[u8]| {
        let mut bytes = len.to_be_bytes().to_vec();
        bytes.extend_from_slice(rest);
        bytes
    };
    let cases = [
        (vec![0u8, 0, 0], DecodeError::UnexpectedEof),
        (frame(0, &[]), DecodeError::LengthOverflow),
        (frame(0x0200_0000, &[0x7F]), DecodeError::LengthOverflow),
        (frame(100, &[0x7F]), DecodeError::UnexpectedEof),
        (frame(9, &[0x7F, 0, 0, 0]), DecodeError::UnexpectedEof),
        (
            frame(1, &[0x42]),
            DecodeError::UnknownFrameKind { tag: 0x42 },
        ),
        (
            frame(9, &[0x40, 0, 0, 0, 0, 0, 0, 0, 0]),
            DecodeError::UnknownFrameKind { tag: 0x40 },
        ),
    ];
    for (bytes, want) in cases {
        assert_eq!(
            Decoder::new(&bytes).read_frame().unwrap_err(),
            want,
            "{bytes:02x?}"
        );
    }
}

/// HELLO bodies from before the protocol-0.7 capability record, or with a
/// truncated record, are malformed. (Unknown values inside a complete
/// record are tolerated: see `hello_growth`.)
#[test]
fn hello_rejects_legacy_and_truncated_capabilities() {
    let full_caps = [
        ColorSupport::TrueColor.as_wire(),
        LayerSet::new().as_wire(),
        ImageProtocolSet::default().as_wire(),
        KeyboardProtocolSet::default().as_wire(),
        1u8,
    ];
    let cases: [(Option<&[u8]>, DecodeError); 4] = [
        (None, DecodeError::UnexpectedEof),
        (Some(&[0x00]), DecodeError::UnexpectedEof),
        (Some(&[0xFF]), DecodeError::UnexpectedEof),
        (Some(&full_caps), DecodeError::UnexpectedEof),
    ];
    for (caps, want) in cases {
        let mut fields = Vec::new();
        tlv_field(&mut fields, 1, b"x");
        tlv_field(&mut fields, 2, &0u16.to_be_bytes());
        tlv_field(&mut fields, 3, &7u16.to_be_bytes());
        tlv_field(&mut fields, 4, &0u16.to_be_bytes());
        if let Some(caps) = caps {
            tlv_field(&mut fields, 5, caps);
        }
        assert_eq!(
            FrameKind::decode(&framed_tlv(0x01, &fields)).unwrap_err(),
            want
        );
    }
}

/// An unknown tag in a nested tagged union that is not a correlated reply's
/// state surfaces `UnknownEnumValue` rather than coercing to a placeholder
/// variant. A spawn or move result's unknown tag decodes as `Unknown`
/// instead (`newer_server.rs`).
#[test]
fn unknown_nested_enum_tags_are_rejected() {
    type Case<'a> = (u8, &'a [(u32, &'a [u8])], &'static str, u32);
    let local0 = [0x00u8, 0, 0, 0, 0];
    let cases: [Case<'_>; 3] = [
        (0x02, &[(1, &[0xFF])], "AttachTarget", 0xFF),
        (0x14, &[(1, &local0), (2, &[0xAB])], "FocusEvent", 0xAB),
        (
            phux_protocol::wire::frame::TYPE_SET_METADATA,
            &[(1, &0u32.to_be_bytes()), (2, &[0xFE])],
            "Scope",
            0xFE,
        ),
    ];
    for (type_byte, tlvs, field, value) in cases {
        let mut fields = Vec::new();
        for (id, bytes) in tlvs {
            tlv_field(&mut fields, *id, bytes);
        }
        assert_eq!(
            FrameKind::decode(&framed_tlv(type_byte, &fields)).unwrap_err(),
            DecodeError::UnknownEnumValue { field, value },
        );
    }
}

/// What a newer peer may send decodes or names its request: an unknown
/// command tag surfaces the `request_id` (whatever the field order), and an
/// unknown `ErrorCode` decodes as `Unknown` carrying its raw value.
#[test]
fn newer_peer_values_are_correlated_or_tolerated() {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 2, &[0x7F]);
    tlv_field(&mut fields, 1, &41u32.to_be_bytes());
    assert_eq!(
        FrameKind::decode(&framed_tlv(0x31, &fields)).unwrap_err(),
        DecodeError::UnsupportedCommand {
            request_id: 41,
            field: "Command",
            value: 0x7F,
        },
    );

    let mut fields = Vec::new();
    tlv_field(&mut fields, 2, &0x9999u16.to_be_bytes());
    tlv_field(&mut fields, 3, b"from the future");
    let (frame, _) = FrameKind::decode(&framed_tlv(0xC1, &fields)).unwrap();
    assert_eq!(
        frame,
        FrameKind::Error {
            request_id: None,
            code: ErrorCode::Unknown(0x9999),
            message: "from the future".to_owned(),
        }
    );
    assert_round_trip(&frame);
}

#[test]
fn layer_set_round_trips_drops_unknown_bits_and_forces_l1() {
    for ls in [
        LayerSet::new(),
        LayerSet::with(&[Layer::L2]),
        LayerSet::with(&[Layer::L3]),
        LayerSet::all(),
    ] {
        let back = LayerSet::from_wire(ls.as_wire());
        assert_eq!(back, ls);
        assert!(back.contains(Layer::L1));
    }
    let future = LayerSet::from_wire(0x80 | 0x04);
    assert_eq!(future, LayerSet::with(&[Layer::L3]));
}

#[test]
fn unknown_field_id_is_skipped_forward_compat() {
    // Forward-compat (`docs/protocol.md#framing`): a decoder MUST skip a
    // field id it does not recognise, by that field's declared length, and
    // decode the rest of the message normally. Encode a real PING (nonce field
    // id 1), then splice in an unknown field id (99) carrying junk *before* the
    // known field; the nonce must still decode and the unknown field is ignored.
    let real = {
        let mut buf = BytesMut::new();
        FrameKind::Ping {
            nonce: 0x0102_0304_0506_0708,
        }
        .encode(&mut buf);
        buf.to_vec()
    };
    // Reconstruct the body with an extra unknown field prepended.
    let type_byte = real[4];
    let known_fields = &real[5..]; // the PING nonce field
    let mut fields = Vec::new();
    tlv_field(&mut fields, 99, &[0xDE, 0xAD, 0xBE, 0xEF]); // unknown field, skipped
    fields.extend_from_slice(known_fields);
    let bytes = framed_tlv(type_byte, &fields);

    let (decoded, tail) = FrameKind::decode(&bytes).unwrap();
    assert_eq!(
        decoded,
        FrameKind::Ping {
            nonce: 0x0102_0304_0506_0708,
        },
        "the unknown field must be skipped and the known field still decode",
    );
    assert!(tail.is_empty());
}

#[test]
fn unknown_trailing_field_id_is_skipped_forward_compat() {
    // The same skip-by-length rule for an unknown field appended *after* the
    // known fields — the shape a newer peer produces when it adds a field an
    // older decoder does not know.
    let real = {
        let mut buf = BytesMut::new();
        FrameKind::Bell {
            terminal_id: ResourceId::local(0x2A),
        }
        .encode(&mut buf);
        buf.to_vec()
    };
    let type_byte = real[4];
    let mut fields = real[5..].to_vec(); // the Bell terminal_id field
    tlv_field(&mut fields, 250, &[1, 2, 3, 4, 5, 6]); // unknown trailing field
    let bytes = framed_tlv(type_byte, &fields);

    let (decoded, tail) = FrameKind::decode(&bytes).unwrap();
    assert_eq!(
        decoded,
        FrameKind::Bell {
            terminal_id: ResourceId::local(0x2A),
        },
    );
    assert!(tail.is_empty());
}

#[test]
fn invalid_utf8_in_hello_client_name() {
    // HELLO (0x01) whose CLIENT_NAME field (id 1) holds non-UTF-8 bytes must
    // surface InvalidUtf8. The client_name value rides as raw bytes inside the
    // length-delimited field (no inner length prefix under TLV).
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &[0xFFu8, 0xFE, 0xFD]); // field::hello::CLIENT_NAME
    let bytes = framed_tlv(0x01, &fields);

    let err = Decoder::new(&bytes).read_frame().unwrap_err();
    assert_eq!(err, DecodeError::InvalidUtf8);
}

#[test]
fn tail_is_returned_after_single_frame() {
    let frame = FrameKind::Ping { nonce: 7 };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf.extend_from_slice(&[0xAA, 0xBB, 0xCC]);

    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(decoded, frame);
    assert_eq!(tail, &[0xAA, 0xBB, 0xCC]);
}

proptest! {

    /// Snapshot entries with resource facets round-trip through the trailing
    /// facet list, and a mix of plain and faceted entries keeps every entry
    /// on the right id.
    #[test]
    fn roundtrip_resource_info(
        infos in proptest::collection::vec(arb_resource_info(), 1..4),
    ) {
        // Dedupe ids: the facet join is by id, so two entries sharing an id
        // would legitimately collapse.
        let mut seen = std::collections::HashSet::new();
        let panes: Vec<ResourceInfo> = infos
            .into_iter()
            .filter(|info| seen.insert(info.id.clone()))
            .collect();
        let snap = SessionSnapshot::new(SessionId::new(0), WindowId::new(0), ResourceId::new(0))
            .with_resources(panes);
        assert_round_trip(&FrameKind::Attached {
            attach_id: 1,
            snapshot: snap,
            initial_client_id: ClientId::new(0),
        });
    }

}

#[test]
fn error_code_wire_values_match_spec() {
    // SPEC §14 names these wire values; lock them in so a refactor cannot
    // silently renumber the enum.
    for (code, wire) in [
        (ErrorCode::VersionIncompatible, 1),
        (ErrorCode::UnknownMessageType, 2),
        (ErrorCode::MalformedMessage, 3),
        (ErrorCode::FrameTooLarge, 4),
        (ErrorCode::NotAttached, 100),
        (ErrorCode::AlreadyAttached, 101),
        (ErrorCode::SessionNotFound, 102),
        (ErrorCode::WindowNotFound, 103),
        (ErrorCode::TerminalNotFound, 104),
        (ErrorCode::ClientNotFound, 105),
        (ErrorCode::UnsupportedSatelliteRoute, 106),
        (ErrorCode::SatelliteUnreachable, 107),
        (ErrorCode::InvalidCommand, 200),
        (ErrorCode::PermissionDenied, 201),
        (ErrorCode::ResourceExhausted, 202),
        (ErrorCode::UnsafePaste, 203),
        (ErrorCode::InputLeaseHeld, 204),
        (ErrorCode::InputDeliveryUnknown, 205),
        (ErrorCode::CanonicalLimitExceeded, 206),
        (ErrorCode::InputNotWritten, 207),
        (ErrorCode::WrongResourceKind, 208),
        (ErrorCode::NotProducer, 209),
        (ErrorCode::RecordInvalid, 210),
        (ErrorCode::Overflow, 211),
        (ErrorCode::InternalError, 65535),
    ] {
        assert_eq!(code.as_wire(), wire, "{code:?}");
    }
}
