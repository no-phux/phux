//! Protocol-0.7 opaque client terminal-emulator reply wire contract.

#![allow(clippy::unwrap_used)]

use bytes::{Bytes, BytesMut};
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ServerCapabilities, ServerFeature, ServerFeatureSet,
};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::{
    FrameKind, MAX_INPUT_TERMINAL_REPLY_BYTES, TYPE_HISTORY_REQUEST, TYPE_INPUT_TERMINAL_REPLY,
};

use crate::common::{
    framed_tlv, local_id_bytes as local_terminal, put_varint, take_varint, tlv_field,
};

fn framed(fields: &[u8]) -> Vec<u8> {
    framed_tlv(TYPE_INPUT_TERMINAL_REPLY, fields)
}

#[test]
fn hello_ok_explicitly_negotiates_terminal_replies() {
    let old_07 = ServerCapabilities::new();
    assert!(!old_07.features.contains(ServerFeature::TerminalReply));

    let negotiated = ServerCapabilities::new()
        .with_features(ServerFeatureSet::with(&[ServerFeature::TerminalReply]));
    let hello_ok = FrameKind::HelloOk {
        protocol_major: 0,
        protocol_minor: 7,
        protocol_patch: 0,
        server_caps: negotiated,
        server_id: b"terminal-reply-server".to_vec(),
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    };
    let mut encoded = BytesMut::new();
    hello_ok.encode(&mut encoded);
    let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
    assert!(tail.is_empty());
    let FrameKind::HelloOk { server_caps, .. } = decoded else {
        panic!("expected HELLO_OK");
    };
    assert!(server_caps.features.contains(ServerFeature::TerminalReply));
}

#[test]
fn discriminator_is_client_originated_and_does_not_reuse_history() {
    assert_eq!(TYPE_INPUT_TERMINAL_REPLY, 0x17);
    assert_eq!(TYPE_HISTORY_REQUEST, 0x16);
    assert_eq!(TYPE_INPUT_TERMINAL_REPLY & 0x80, 0);

    let frame = FrameKind::InputTerminalReply {
        terminal_id: ResourceId::local(1),
        bytes: Bytes::from_static(b"reply"),
    };
    assert_eq!(frame.type_byte(), TYPE_INPUT_TERMINAL_REPLY);
}

#[test]
fn opaque_nul_escape_and_non_utf8_bytes_round_trip_exactly() {
    let opaque = Bytes::from_static(b"\0\x1b[?1;2c\xff\x80\x1b]10;?\x07");
    let frame = FrameKind::InputTerminalReply {
        terminal_id: ResourceId::local(0x1020_3040),
        bytes: opaque.clone(),
    };
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
    assert!(tail.is_empty());
    assert_eq!(decoded, frame);
    let FrameKind::InputTerminalReply { bytes, .. } = decoded else {
        panic!("expected INPUT_TERMINAL_REPLY");
    };
    assert_eq!(bytes, opaque);
}

#[test]
fn fields_are_required_and_encoded_in_allocated_order() {
    let frame = FrameKind::InputTerminalReply {
        terminal_id: ResourceId::local(7),
        bytes: Bytes::from_static(b"\x1b[0n"),
    };
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    let mut offset = 5;
    let mut ids = Vec::new();
    while offset < encoded.len() {
        ids.push(take_varint(&encoded, &mut offset));
        assert_eq!(encoded[offset], 4);
        offset += 1;
        let len = usize::try_from(take_varint(&encoded, &mut offset)).unwrap();
        offset += len;
    }
    assert_eq!(ids, [1, 2]);

    let mut only_terminal = Vec::new();
    tlv_field(&mut only_terminal, 1, &local_terminal(7));
    assert_eq!(
        FrameKind::decode(&framed(&only_terminal)).unwrap_err(),
        DecodeError::UnexpectedEof
    );

    let mut only_bytes = Vec::new();
    tlv_field(&mut only_bytes, 2, b"reply");
    assert_eq!(
        FrameKind::decode(&framed(&only_bytes)).unwrap_err(),
        DecodeError::UnexpectedEof
    );
}

#[test]
fn maximum_sized_reply_is_accepted() {
    let frame = FrameKind::InputTerminalReply {
        terminal_id: ResourceId::local(1),
        bytes: Bytes::from(vec![0xA5; MAX_INPUT_TERMINAL_REPLY_BYTES]),
    };
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
    assert!(tail.is_empty());
    assert_eq!(decoded, frame);
}

#[test]
fn empty_and_oversized_replies_are_rejected_before_dispatch() {
    for bytes in [
        Bytes::new(),
        Bytes::from(vec![0xA5; MAX_INPUT_TERMINAL_REPLY_BYTES + 1]),
    ] {
        let frame = FrameKind::InputTerminalReply {
            terminal_id: ResourceId::local(1),
            bytes,
        };
        let mut encoded = BytesMut::new();
        frame.encode(&mut encoded);
        assert_eq!(
            FrameKind::decode(&encoded).unwrap_err(),
            DecodeError::InputTerminalReplyLimitExceeded
        );
    }
}

#[test]
fn unknown_fields_are_skipped_without_touching_opaque_reply() {
    let frame = FrameKind::InputTerminalReply {
        terminal_id: ResourceId::local(9),
        bytes: Bytes::from_static(b"\xff\0\x1bPfuture-reply\x1b\\"),
    };
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    let mut unknown = Vec::new();
    tlv_field(&mut unknown, 99, b"future-field");
    encoded.extend_from_slice(&unknown);
    let length = u32::from_be_bytes(encoded[..4].try_into().unwrap())
        .checked_add(u32::try_from(unknown.len()).unwrap())
        .unwrap();
    encoded[..4].copy_from_slice(&length.to_be_bytes());

    let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
    assert!(tail.is_empty());
    assert_eq!(decoded, frame);
}

#[test]
fn malformed_reply_field_length_is_rejected_without_utf8_interpretation() {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &local_terminal(1));
    put_varint(&mut fields, 2);
    fields.push(4);
    put_varint(&mut fields, 32);
    fields.extend_from_slice(b"\xff\0");
    assert_eq!(
        FrameKind::decode(&framed(&fields)).unwrap_err(),
        DecodeError::UnexpectedEof
    );
}
