//! Bounded command payloads, server features, detach reasons, and command values.

use super::{
    BootstrapLimits, BootstrapProfile, BytesMut, Command, CommandResult, CommandValue, DecodeError,
    DetachReason, FileChunk, FileUploadAck, FileUploadId, FocusEvent, FrameKind, InputEvent,
    InputOperationId, LayerSet, MAX_APPLY_INPUT_COMMAND_BODY, MAX_APPLY_INPUT_EVENTS,
    MAX_FILE_UPLOAD_CHUNK, MAX_FILE_UPLOAD_SIZE, PasteEvent, PasteTrust, ResourceId,
    ServerCapabilities, ServerFeature, ServerFeatureSet, SessionId, SessionInfo, SessionSnapshot,
    WindowId, assert_round_trip, framed_tlv, tlv_field,
};

#[test]
fn command_apply_input_round_trips_and_rejects_malformed_payloads() {
    let operation_id = InputOperationId::new([0x5a; 16]).unwrap();
    let frame = FrameKind::Command {
        request_id: 0x0102_0304,
        command: Command::ApplyInput {
            operation_id,
            terminal_id: ResourceId::local(5),
            events: vec![
                InputEvent::Focus(FocusEvent::Gained),
                InputEvent::Paste(PasteEvent {
                    trust: PasteTrust::Trusted,
                    data: b"hello".to_vec(),
                }),
            ],
        },
    };
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    assert_eq!(
        encoded.as_ref(),
        // Each event is a record: `u32` length, then tag and body.
        &[
            0x00, 0x00, 0x00, 0x38, 0x31, 0x01, 0x04, 0x04, 0x01, 0x02, 0x03, 0x04, 0x02, 0x04,
            0x2d, 0x14, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a,
            0x5a, 0x5a, 0x5a, 0x5a, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x02, 0x00, 0x00, 0x00,
            0x02, 0x02, 0x00, 0x00, 0x00, 0x00, 0x0b, 0x03, 0x00, 0x00, 0x00, 0x00, 0x05, 0x68,
            0x65, 0x6c, 0x6c, 0x6f,
        ],
        "APPLY_INPUT wire bytes are stable"
    );
    let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());

    let id_offset = encoded
        .windows(16)
        .position(|window| window == [0x5a; 16])
        .expect("operation id bytes");

    let mut zero_id = encoded.to_vec();
    zero_id[id_offset..id_offset + 16].fill(0);
    assert_eq!(
        FrameKind::decode(&zero_id).unwrap_err(),
        DecodeError::InvalidInputOperationId
    );

    // Local ResourceId is tag + u32, then u16 count, then the first event's
    // record length; mutate that event's tag. A newer event kind is refused
    // by name, with the request it belongs to.
    let mut unknown_event = encoded.to_vec();
    unknown_event[id_offset + 16 + 5 + 2 + 4] = 0xff;
    assert_eq!(
        FrameKind::decode(&unknown_event).unwrap_err(),
        DecodeError::UnsupportedCommand {
            request_id: 0x0102_0304,
            field: "InputEvent",
            value: 0xff,
        }
    );

    let count_offset = id_offset + 16 + 5;
    let mut over_count = encoded.to_vec();
    over_count[count_offset..count_offset + 2].copy_from_slice(
        &u16::try_from(MAX_APPLY_INPUT_EVENTS + 1)
            .unwrap()
            .to_be_bytes(),
    );
    assert_eq!(
        FrameKind::decode(&over_count).unwrap_err(),
        DecodeError::ApplyInputLimitExceeded
    );

    assert!(matches!(
        FrameKind::decode(&encoded[..id_offset + 8]),
        Err(DecodeError::UnexpectedEof)
    ));

    let oversized = FrameKind::Command {
        request_id: 1,
        command: Command::ApplyInput {
            operation_id,
            terminal_id: ResourceId::local(5),
            events: vec![InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Trusted,
                data: vec![b'x'; MAX_APPLY_INPUT_COMMAND_BODY],
            })],
        },
    };
    let mut oversized_bytes = BytesMut::new();
    oversized.encode(&mut oversized_bytes);
    assert_eq!(
        FrameKind::decode(&oversized_bytes).unwrap_err(),
        DecodeError::ApplyInputLimitExceeded
    );
}

#[test]
fn command_put_file_and_ack_round_trip_with_limits() {
    let upload_id = FileUploadId::new([0x6b; 16]).unwrap();
    let frame = FrameKind::Command {
        request_id: 0x0a0b_0c0d,
        command: Command::PutFile {
            upload_id,
            terminal_id: ResourceId::satellite("mini", 5),
            extension: "png".to_owned(),
            offset: 4,
            data: b"tail".to_vec(),
            final_chunk: true,
            sha256: Some([0x7c; 32]),
        },
    };
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());

    let ack = FrameKind::CommandResult {
        request_id: 0x0a0b_0c0d,
        result: CommandResult::OkWith(CommandValue::FileUpload(FileUploadAck {
            next_offset: 8,
            path: Some("/home/u/.local/share/phux/uploads/phux-upload-id.png".to_owned()),
        })),
    };
    assert_round_trip(&ack);

    let id_offset = encoded
        .windows(16)
        .position(|window| window == [0x6b; 16])
        .expect("upload id bytes");
    let mut zero_id = encoded.to_vec();
    zero_id[id_offset..id_offset + 16].fill(0);
    assert_eq!(
        FrameKind::decode(&zero_id).unwrap_err(),
        DecodeError::InvalidFileUploadId
    );

    let over_total = FrameKind::Command {
        request_id: 1,
        command: Command::PutFile {
            upload_id,
            terminal_id: ResourceId::local(5),
            extension: "png".to_owned(),
            offset: MAX_FILE_UPLOAD_SIZE,
            data: vec![1],
            final_chunk: false,
            sha256: None,
        },
    };
    let mut over_total_bytes = BytesMut::new();
    over_total.encode(&mut over_total_bytes);
    assert_eq!(
        FrameKind::decode(&over_total_bytes).unwrap_err(),
        DecodeError::FileUploadLimitExceeded
    );

    let over_chunk = FrameKind::Command {
        request_id: 2,
        command: Command::PutFile {
            upload_id,
            terminal_id: ResourceId::local(5),
            extension: "jpg".to_owned(),
            offset: 0,
            data: vec![0; MAX_FILE_UPLOAD_CHUNK + 1],
            final_chunk: false,
            sha256: None,
        },
    };
    let mut over_chunk_bytes = BytesMut::new();
    over_chunk.encode(&mut over_chunk_bytes);
    assert_eq!(
        FrameKind::decode(&over_chunk_bytes).unwrap_err(),
        DecodeError::FileUploadLimitExceeded
    );
}

#[test]
fn get_file_and_file_chunk_round_trip_within_the_chunk_cap() {
    let frame = FrameKind::Command {
        request_id: 0x0a0b_0c0e,
        command: Command::GetFile {
            host: Some("mini".to_owned()),
            path: "/Users/me/Decks/talk.pptx".to_owned(),
            offset: 8 * 1024 * 1024,
            max_bytes: 4096,
        },
    };
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());

    for sha256 in [None, Some([0x7d; 32])] {
        let reply = FrameKind::CommandResult {
            request_id: 0x0a0b_0c0e,
            result: CommandResult::OkWith(CommandValue::FileChunk(FileChunk {
                size: 9_000_000,
                data: b"chunk".to_vec(),
                sha256,
            })),
        };
        let mut encoded = BytesMut::new();
        reply.encode(&mut encoded);
        let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
        assert_eq!(decoded, reply);
        assert!(tail.is_empty());
    }

    let over_request = FrameKind::Command {
        request_id: 3,
        command: Command::GetFile {
            host: None,
            path: "/x".to_owned(),
            offset: 0,
            max_bytes: u32::try_from(MAX_FILE_UPLOAD_CHUNK + 1).unwrap(),
        },
    };
    let mut over_request_bytes = BytesMut::new();
    over_request.encode(&mut over_request_bytes);
    assert_eq!(
        FrameKind::decode(&over_request_bytes).unwrap_err(),
        DecodeError::FileUploadLimitExceeded
    );

    let over_reply = FrameKind::CommandResult {
        request_id: 4,
        result: CommandResult::OkWith(CommandValue::FileChunk(FileChunk {
            size: 0,
            data: vec![0; MAX_FILE_UPLOAD_CHUNK + 1],
            sha256: None,
        })),
    };
    let mut over_reply_bytes = BytesMut::new();
    over_reply.encode(&mut over_reply_bytes);
    assert_eq!(
        FrameKind::decode(&over_reply_bytes).unwrap_err(),
        DecodeError::FileUploadLimitExceeded
    );
}

#[test]
fn hello_ok_server_feature_round_trips_and_old_caps_default_empty() {
    let frame = FrameKind::HelloOk {
        protocol_major: 0,
        protocol_minor: 5,
        protocol_patch: 0,
        server_caps: ServerCapabilities::new()
            .with_layers(LayerSet::all())
            .with_features(ServerFeatureSet::with(&[ServerFeature::AcknowledgedInput])),
        server_id: vec![],
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    };
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    assert!(
        encoded
            .windows(5)
            .any(|window| window == [LayerSet::all().as_wire(), 0, 0, 0, 0x10]),
        "server feature bits must trail the one-byte layer set"
    );
    let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
    assert_eq!(decoded, frame);
    assert!(tail.is_empty());

    let old = ServerCapabilities::new().with_layers(LayerSet::all());
    assert!(old.features.is_empty());
}

/// The reason-less `DETACHED` is byte-identical to the empty body every
/// pre-`0.7.0-draft.7` peer emits, in both directions (ADR-0061).
#[test]
fn detached_without_a_reason_is_the_legacy_empty_body() {
    let unstated = FrameKind::Detached {
        reason: None,
        message: String::new(),
    };
    let mut buf = BytesMut::new();
    unstated.encode(&mut buf);
    assert_eq!(buf.as_ref(), framed_tlv(0x82, &[]).as_slice());
    assert_round_trip(&unstated);
}

/// A reason value allocated by a *later* draft must decode as unstated rather
/// than failing the frame: `DETACHED` plus transport close is the only
/// termination signal a consumer may act on (docs/protocol.md), so rejecting it
/// would turn an explained ending into an unexplained disconnect — and would
/// make every future `DetachReason` a fleet-wide break.
#[test]
fn detached_unknown_reason_decodes_as_unstated_and_keeps_the_message() {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &[0x7f]); // field::detached::REASON, unallocated
    tlv_field(&mut fields, 2, b"from the future"); // field::detached::MESSAGE
    let buf = framed_tlv(0x82, &fields);

    let (decoded, tail) = FrameKind::decode(&buf).unwrap();
    assert_eq!(
        decoded,
        FrameKind::Detached {
            reason: None,
            message: "from the future".to_owned(),
        },
        "an unrecognised reason must degrade to unstated, not fail the frame"
    );
    assert!(tail.is_empty());
}

/// `DetachReason` wire values are normative (docs/protocol.md#attach-and-output). Pin them here so
/// a reordering of the enum cannot silently renumber the wire.
#[test]
fn detach_reason_wire_values_match_spec() {
    assert_eq!(DetachReason::Requested.as_wire(), 0);
    assert_eq!(DetachReason::ServerShutdown.as_wire(), 1);
    assert_eq!(DetachReason::SessionKilled.as_wire(), 2);
    assert_eq!(DetachReason::Replaced.as_wire(), 3);
    assert_eq!(DetachReason::ProtocolError.as_wire(), 4);
    assert_eq!(DetachReason::AuthenticationFailed.as_wire(), 5);
    assert_eq!(DetachReason::AuthorizationRevoked.as_wire(), 6);
    assert_eq!(DetachReason::AuthorizationExpired.as_wire(), 7);
    assert_eq!(DetachReason::InternalError.as_wire(), 255);
    for known in [5u8, 6, 7] {
        assert_eq!(
            DetachReason::from_wire(known).map(DetachReason::as_wire),
            Some(known),
            "workload-auth §7 reasons decode"
        );
    }
    for unallocated in [8u8, 42, 254] {
        assert_eq!(DetachReason::from_wire(unallocated), None);
    }
}

/// `COMMAND_RESULT`'s `OkWith` payloads share one wire shape; the table
/// covers `GET_SCREEN`'s JSON reply, `GET_STATE`'s snapshot reply (the
/// ATTACHED snapshot shape, so a non-trivial snapshot must survive), and the
/// `ResourceId` reply.
#[test]
fn command_result_ok_with_values_round_trip() {
    let info = SessionInfo::new(SessionId::new(1), "work".to_owned());
    let snap = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_sessions(vec![info]);
    for value in [
        CommandValue::Json(
            r#"{"schema_version":1,"pane":5,"cols":80,"rows":24,"cursor":null,"lines":["$ "]}"#
                .to_owned(),
        ),
        CommandValue::State(snap),
        CommandValue::ResourceId(ResourceId::local(42)),
    ] {
        assert_round_trip(&FrameKind::CommandResult {
            request_id: 12,
            result: CommandResult::OkWith(value),
        });
    }
}
