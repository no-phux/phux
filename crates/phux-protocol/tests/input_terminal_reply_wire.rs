//! Protocol-0.7 opaque client terminal-emulator reply wire contract.

#![allow(clippy::unwrap_used)]

use bytes::{Bytes, BytesMut};
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::{
    FrameKind, MAX_INPUT_TERMINAL_REPLY_BYTES, TYPE_HISTORY_REQUEST, TYPE_INPUT_TERMINAL_REPLY,
};

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

fn framed(fields: &[u8]) -> Vec<u8> {
    let length = 1usize.checked_add(fields.len()).unwrap();
    let mut out = Vec::with_capacity(length + 4);
    out.extend_from_slice(&u32::try_from(length).unwrap().to_be_bytes());
    out.push(TYPE_INPUT_TERMINAL_REPLY);
    out.extend_from_slice(fields);
    out
}

fn local_terminal(raw: u32) -> [u8; 5] {
    let mut value = [0_u8; 5];
    value[1..].copy_from_slice(&raw.to_be_bytes());
    value
}

fn take_varint(input: &[u8], offset: &mut usize) -> u64 {
    let mut value = 0_u64;
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

#[test]
fn discriminator_is_client_originated_and_does_not_reuse_history() {
    assert_eq!(TYPE_INPUT_TERMINAL_REPLY, 0x17);
    assert_eq!(TYPE_HISTORY_REQUEST, 0x16);
    assert_eq!(TYPE_INPUT_TERMINAL_REPLY & 0x80, 0);

    let frame = FrameKind::InputTerminalReply {
        terminal_id: TerminalId::local(1),
        bytes: Bytes::from_static(b"reply"),
    };
    assert_eq!(frame.type_byte(), TYPE_INPUT_TERMINAL_REPLY);
}

#[test]
fn opaque_nul_escape_and_non_utf8_bytes_round_trip_exactly() {
    let opaque = Bytes::from_static(b"\0\x1b[?1;2c\xff\x80\x1b]10;?\x07");
    let frame = FrameKind::InputTerminalReply {
        terminal_id: TerminalId::local(0x1020_3040),
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
        terminal_id: TerminalId::local(7),
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
        terminal_id: TerminalId::local(1),
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
            terminal_id: TerminalId::local(1),
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
        terminal_id: TerminalId::local(9),
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
