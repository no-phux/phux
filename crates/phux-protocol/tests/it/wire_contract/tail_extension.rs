//! Protocol 1.x growth rules, as an older decoder sees a newer peer's bytes.
//!
//! A positional value grows only at its end. A record that is not the last
//! element of its field is `u32 length || body`, so bytes a newer encoder
//! appends to the body are skipped; a value that ends its field grows the
//! same way because a field's trailing bytes are ignored. An unknown
//! `COMMAND_RESULT` or `CommandValue` tag, or an unknown enum inside a known
//! value, ends its field too, so the rest is skipped and the request settles
//! as an [`ErrorCode::UNREADABLE_REPLY`] error.

#![allow(clippy::unwrap_used, clippy::panic, reason = "tests")]

use phux_protocol::ids::{IdempotencyKey, ResourceId};
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::wire::decode::Decoder;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind, KillPrecondition};

use crate::common::{decode, encode, framed_tlv, record, tlv_field};

const TYPE_COMMAND: u8 = 0x31;
const TYPE_COMMAND_RESULT: u8 = 0xC2;
/// Bytes of a local `ResourceId`: tag, then `u32`.
const LOCAL_ID_LEN: usize = 5;

/// The value of field `id` in an encoded frame.
fn field(bytes: &[u8], id: u32) -> Vec<u8> {
    let mut dec = Decoder::new(&bytes[5..]);
    while let Some((field_id, value)) = dec.read_field().unwrap() {
        if field_id == id {
            return value.to_vec();
        }
    }
    panic!("no field {id}");
}

/// A frame of `type_byte` carrying `request_id` (field 1) and `body` (2).
fn frame(type_byte: u8, request_id: u32, body: &[u8]) -> Vec<u8> {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &request_id.to_be_bytes());
    tlv_field(&mut fields, 2, body);
    framed_tlv(type_byte, &fields)
}

/// `body` with the record at `at` grown by `extra` bytes at its end.
fn grow_record(body: &[u8], at: usize, extra: &[u8]) -> Vec<u8> {
    let len = usize::try_from(u32::from_be_bytes(body[at..at + 4].try_into().unwrap())).unwrap();
    let end = at + 4 + len;
    let mut grown = body[at + 4..end].to_vec();
    grown.extend_from_slice(extra);
    [&body[..at], record(&grown).as_slice(), &body[end..]].concat()
}

#[test]
fn an_input_event_record_grows_without_breaking_its_command() {
    let route = FrameKind::Command {
        request_id: 3,
        command: Command::RouteInput {
            terminal_id: ResourceId::local(5),
            event: InputEvent::Focus(FocusEvent::Lost),
        },
    };
    let body = field(&encode(&route), 2);
    // Command tag, terminal id, then the event record.
    let grown = grow_record(&body, 1 + LOCAL_ID_LEN, &[0xEE, 0xEE, 0xEE]);
    assert_ne!(grown, body);
    assert_eq!(decode(&frame(TYPE_COMMAND, 3, &grown)).unwrap(), route);
}

#[test]
fn a_grown_kill_precondition_keeps_the_trailing_key_in_place() {
    let kill = FrameKind::Command {
        request_id: 4,
        command: Command::KillResourceIf {
            terminal_id: ResourceId::local(7),
            precondition: KillPrecondition::default(),
            operation_id: IdempotencyKey::new([0x5a; 16]),
        },
    };
    let body = field(&encode(&kill), 2);
    let grown = grow_record(&body, 1 + LOCAL_ID_LEN, &[1, 2, 3, 4]);
    assert_eq!(decode(&frame(TYPE_COMMAND, 4, &grown)).unwrap(), kill);
}

#[test]
fn unknown_result_and_value_tags_settle_the_request_as_unreadable() {
    let cases = [
        (vec![0xEE, 1, 2, 3], "CommandResult tag 0xee"),
        // OK_WITH, then a value tag no build defines yet.
        (vec![0x01, 0xEE, 9, 9], "CommandValue tag 0xee"),
        (vec![0xEF], "CommandResult tag 0xef"),
        // OK_WITH, a resource-id value, then an id tag no build defines: an
        // unknown enum inside a known value settles the request the same way.
        (vec![0x01, 0x00, 0xFF], "ResourceId tag 0xff"),
    ];
    for (body, named) in cases {
        let decoded = decode(&frame(TYPE_COMMAND_RESULT, 8, &body)).unwrap();
        let FrameKind::CommandResult {
            request_id: 8,
            result: CommandResult::Error { code, message },
        } = decoded
        else {
            panic!("{body:?} decoded as {decoded:?}");
        };
        // Never `Unsupported`: the server answered, so the request may have
        // taken effect.
        assert_eq!(code, ErrorCode::UNREADABLE_REPLY);
        assert_ne!(code, ErrorCode::Unsupported);
        assert!(message.contains(named), "{message:?}");
    }
    // A truncated value is still malformed.
    assert!(decode(&frame(TYPE_COMMAND_RESULT, 8, &[0x01, 0x00, 0x00, 0x00])).is_err());
}

#[test]
fn a_command_error_with_a_newer_code_decodes() {
    // ERROR tag, code 0x0FFF, then an empty message.
    let body = [0x02, 0x0F, 0xFF, 0, 0, 0, 0];
    assert_eq!(
        decode(&frame(TYPE_COMMAND_RESULT, 2, &body)).unwrap(),
        FrameKind::CommandResult {
            request_id: 2,
            result: CommandResult::Error {
                code: ErrorCode::Unknown(0x0FFF),
                message: String::new(),
            },
        }
    );
}
