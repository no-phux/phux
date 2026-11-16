//! Protocol-0.7 opaque client terminal-emulator reply wire contract.

#![allow(clippy::unwrap_used)]

use bytes::Bytes;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ServerCapabilities, ServerFeature, ServerFeatureSet,
};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::{
    FrameKind, MAX_INPUT_TERMINAL_REPLY_BYTES, TYPE_HISTORY_REQUEST, TYPE_INPUT_TERMINAL_REPLY,
};

use crate::common::{
    assert_round_trip, decode, encode, encode_with_unknown_field, framed_tlv,
    local_id_bytes as local_terminal, put_varint, take_varint, tlv_field,
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
        selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
        bootstrap_limits: BootstrapLimits::default(),
    };
    let FrameKind::HelloOk { server_caps, .. } = decode(&encode(&hello_ok)).unwrap() else {
        panic!("expected HELLO_OK");
    };
    assert!(server_caps.features.contains(ServerFeature::TerminalReply));
}

const fn reply(terminal: u32, bytes: Bytes) -> FrameKind {
    FrameKind::InputTerminalReply {
        terminal_id: ResourceId::local(terminal),
        bytes,
    }
}

#[test]
fn discriminator_is_client_originated_and_does_not_reuse_history() {
    assert_eq!(TYPE_INPUT_TERMINAL_REPLY, 0x17);
    assert_eq!(TYPE_HISTORY_REQUEST, 0x16);
    assert_eq!(TYPE_INPUT_TERMINAL_REPLY & 0x80, 0);
    let frame = reply(1, Bytes::from_static(b"reply"));
    assert_eq!(frame.type_byte(), TYPE_INPUT_TERMINAL_REPLY);
}

/// Opaque NUL, escape and non-UTF-8 bytes round-trip exactly, up to the
/// maximum reply size; an unknown trailing field is skipped without
/// touching them.
#[test]
fn opaque_nul_escape_and_non_utf8_bytes_round_trip_exactly() {
    for frame in [
        reply(
            0x1020_3040,
            Bytes::from_static(b"\0\x1b[?1;2c\xff\x80\x1b]10;?\x07"),
        ),
        reply(1, Bytes::from(vec![0xA5; MAX_INPUT_TERMINAL_REPLY_BYTES])),
    ] {
        assert_round_trip(&frame);
    }
    let frame = reply(9, Bytes::from_static(b"\xff\0\x1bPfuture-reply\x1b\\"));
    let encoded = encode_with_unknown_field(&frame, 99, b"future-field");
    assert_eq!(decode(&encoded).unwrap(), frame);
}

#[test]
fn fields_are_required_and_encoded_in_allocated_order() {
    let encoded = encode(&reply(7, Bytes::from_static(b"\x1b[0n")));
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

    for (id, value) in [(1, local_terminal(7)), (2, b"reply".to_vec())] {
        let mut only = Vec::new();
        tlv_field(&mut only, id, &value);
        assert_eq!(
            FrameKind::decode(&framed(&only)).unwrap_err(),
            DecodeError::UnexpectedEof,
            "only field {id}"
        );
    }
}

#[test]
fn empty_and_oversized_replies_are_rejected_before_dispatch() {
    for bytes in [
        Bytes::new(),
        Bytes::from(vec![0xA5; MAX_INPUT_TERMINAL_REPLY_BYTES + 1]),
    ] {
        let len = bytes.len();
        assert_eq!(
            FrameKind::decode(&encode(&reply(1, bytes))).unwrap_err(),
            DecodeError::InputTerminalReplyLimitExceeded,
            "{len}-byte reply"
        );
    }
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
