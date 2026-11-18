//! Protocol-0.7 opaque client terminal-emulator reply wire contract.

#![allow(clippy::unwrap_used)]

use bytes::Bytes;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::{
    FrameKind, MAX_INPUT_TERMINAL_REPLY_BYTES, TYPE_INPUT_TERMINAL_REPLY,
};

use crate::common::{
    assert_round_trip, encode, framed_tlv, local_id_bytes as local_terminal, put_varint,
    take_varint, tlv_field,
};

fn framed(fields: &[u8]) -> Vec<u8> {
    framed_tlv(TYPE_INPUT_TERMINAL_REPLY, fields)
}

const fn reply(terminal: u32, bytes: Bytes) -> FrameKind {
    FrameKind::InputTerminalReply {
        terminal_id: ResourceId::local(terminal),
        bytes,
    }
}

#[test]
fn discriminator_is_0x17() {
    assert_eq!(TYPE_INPUT_TERMINAL_REPLY, 0x17);
}

/// Opaque NUL, escape and non-UTF-8 bytes round-trip exactly, up to the
/// maximum reply size.
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
fn an_empty_reply_is_rejected_before_dispatch() {
    assert_eq!(
        FrameKind::decode(&encode(&reply(1, Bytes::new()))).unwrap_err(),
        DecodeError::InputTerminalReplyLimitExceeded
    );
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
