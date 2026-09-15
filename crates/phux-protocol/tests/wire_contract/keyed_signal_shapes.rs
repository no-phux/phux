//! Keyed supervisory commands (`docs/spec/L1.md` §5.1.1): the trailing
//! `operation_id: bytes16` on `KILL_RESOURCE`, `KILL_RESOURCE_IF`,
//! `KILL_RESOURCES`, and `SIGNAL_TERMINAL`, the `KEYED_SIGNAL` bit, and
//! `INCARNATION_CHANGED`.
//!
//! The goldens are built by hand from the spec layout, not from the encoder,
//! so an unkeyed command is pinned to the bytes every encoder before the
//! field wrote. A keyed command is that body with sixteen bytes appended
//! inside the length-delimited `COMMAND` field, which is why an older
//! decoder, which stops after the last field it knows, never reads them.

#![allow(clippy::unwrap_used, clippy::panic, reason = "tests")]

use bytes::BytesMut;
use phux_protocol::caps::{KEYED_SIGNAL, ServerFeature, ServerFeatureSet};
use phux_protocol::ids::{IdempotencyKey, ResourceId};
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, KillPrecondition, TerminalSignal,
};

use crate::common::{framed_tlv, tlv_field};

const TYPE_COMMAND: u8 = 0x31;
const TYPE_COMMAND_RESULT: u8 = 0xC2;

fn encode(frame: &FrameKind) -> Vec<u8> {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf.to_vec()
}

fn decode(bytes: &[u8]) -> Result<FrameKind, DecodeError> {
    let (frame, tail) = FrameKind::decode(bytes)?;
    assert!(tail.is_empty(), "decoder left {} bytes", tail.len());
    Ok(frame)
}

/// A `COMMAND` frame with `request_id` 9 around the positional `body`.
fn command_frame(body: &[u8]) -> Vec<u8> {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &9u32.to_be_bytes());
    tlv_field(&mut fields, 2, body);
    framed_tlv(TYPE_COMMAND, &fields)
}

const fn command(command: Command) -> FrameKind {
    FrameKind::Command {
        request_id: 9,
        command,
    }
}

const fn key() -> IdempotencyKey {
    IdempotencyKey::new([0x5a; 16]).unwrap()
}

/// `(unkeyed command, its positional body as written before the key)`.
fn unkeyed_goldens() -> Vec<(Command, Vec<u8>)> {
    vec![
        (
            Command::KillResource {
                terminal_id: ResourceId::local(7),
                operation_id: None,
            },
            vec![0x03, 0x00, 0, 0, 0, 7],
        ),
        (
            Command::KillResourceIf {
                terminal_id: ResourceId::local(7),
                precondition: KillPrecondition::default(),
                operation_id: None,
            },
            // tag, id, `instance: None`, `conditions: 0`
            vec![0x1b, 0x00, 0, 0, 0, 7, 0x00, 0x00],
        ),
        (
            Command::KillResources {
                ids: vec![ResourceId::local(1), ResourceId::local(2)],
                operation_id: None,
            },
            vec![0x09, 0x00, 0x02, 0x00, 0, 0, 0, 1, 0x00, 0, 0, 0, 2],
        ),
        (
            Command::SignalTerminal {
                terminal_id: ResourceId::local(3),
                signal: TerminalSignal::Interrupt,
                operation_id: None,
            },
            vec![0x11, 0x00, 0, 0, 0, 3, 0x00],
        ),
    ]
}

/// The same command carrying [`key`].
fn keyed(command: &Command) -> Command {
    let mut keyed = command.clone();
    match &mut keyed {
        Command::KillResource { operation_id, .. }
        | Command::KillResourceIf { operation_id, .. }
        | Command::KillResources { operation_id, .. }
        | Command::SignalTerminal { operation_id, .. } => *operation_id = Some(key()),
        other => panic!("not a keyed supervisory command: {other:?}"),
    }
    keyed
}

#[test]
fn unkeyed_supervisory_commands_keep_their_pre_key_bytes() {
    for (unkeyed, body) in unkeyed_goldens() {
        let frame = command(unkeyed.clone());
        assert_eq!(
            encode(&frame),
            command_frame(&body),
            "{unkeyed:?} changed bytes"
        );
        assert_eq!(decode(&command_frame(&body)).unwrap(), frame);
        assert_eq!(unkeyed.idempotency_key(), None);
    }
}

#[test]
fn a_keyed_command_is_the_unkeyed_body_plus_sixteen_trailing_bytes() {
    for (unkeyed, mut body) in unkeyed_goldens() {
        let keyed = keyed(&unkeyed);
        body.extend_from_slice(key().as_bytes());
        // The key lives inside field 2's length, so nothing past the
        // command field moves: an older decoder bounded by that length
        // reads the fields it knows and never sees the key.
        assert_eq!(
            encode(&command(keyed.clone())),
            command_frame(&body),
            "{keyed:?}"
        );
        let decoded = decode(&command_frame(&body)).unwrap();
        assert_eq!(decoded, command(keyed.clone()));
        assert_eq!(keyed.idempotency_key(), Some(&key()));
    }
}

#[test]
fn a_zero_or_truncated_trailing_key_is_malformed() {
    for (_, body) in unkeyed_goldens() {
        let mut zero = body.clone();
        zero.extend_from_slice(&[0; 16]);
        assert_eq!(
            decode(&command_frame(&zero)).unwrap_err(),
            DecodeError::InvalidIdempotencyKey,
            "an all-zero key is not a key"
        );
        let mut short = body;
        short.extend_from_slice(&[0x5a; 5]);
        assert!(
            decode(&command_frame(&short)).is_err(),
            "a partial key is not a key"
        );
    }
}

#[test]
fn incarnation_changed_is_request_scoped_code_213() {
    let frame = FrameKind::CommandResult {
        request_id: 9,
        result: CommandResult::Error {
            code: ErrorCode::IncarnationChanged,
            message: String::new(),
        },
    };
    let mut result = vec![0x02]; // COMMAND_RESULT_TAG_ERROR
    result.extend_from_slice(&213u16.to_be_bytes());
    result.extend_from_slice(&0u32.to_be_bytes()); // empty message
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &9u32.to_be_bytes());
    tlv_field(&mut fields, 2, &result);
    let golden = framed_tlv(TYPE_COMMAND_RESULT, &fields);
    assert_eq!(encode(&frame), golden);
    assert_eq!(decode(&golden).unwrap(), frame);
    assert_eq!(
        ErrorCode::IncarnationChanged.scope(),
        phux_protocol::wire::frame::ErrorScope::Request
    );
}

#[test]
fn keyed_signal_is_bit_0x20000000() {
    assert_eq!(KEYED_SIGNAL, 0x2000_0000);
    let features = ServerFeatureSet::from_wire(KEYED_SIGNAL);
    assert!(features.contains(ServerFeature::KeyedSignal));
    assert_eq!(features.as_wire(), KEYED_SIGNAL);
}
