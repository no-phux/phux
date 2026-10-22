//! Keyed supervisory commands (`docs/protocol.md#receipts-retries-and-backpressure`): the optional
//! `operation_id: bytes16` on `KILL_RESOURCE`, `KILL_RESOURCE_IF`,
//! `KILL_RESOURCES`, and `SIGNAL_TERMINAL`, the `KEYED_SIGNAL` bit, and
//! `INCARNATION_CHANGED`.
//!
//! The goldens are built by hand from the spec layout, not from the encoder.
//! The key sits behind a presence byte (`0` unkeyed, `1` then sixteen
//! bytes), never inferred from the bytes left, so a field a newer build
//! appends after it is skipped by this decoder instead of misread as a key.

#![allow(clippy::unwrap_used, clippy::panic, reason = "tests")]

use phux_protocol::caps::{KEYED_SIGNAL, ServerFeature, ServerFeatureSet};
use phux_protocol::ids::{IdempotencyKey, ResourceId};
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, KillPrecondition, TerminalSignal,
};

use crate::common::{decode, encode, framed_tlv, tlv_field};

const TYPE_COMMAND: u8 = 0x31;
const TYPE_COMMAND_RESULT: u8 = 0xC2;

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

/// `(unkeyed command, its positional body up to the key's presence byte)`.
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
            // tag, id, then the precondition record: `u32` length 2,
            // `instance: None`, `conditions: 0`
            vec![0x1b, 0x00, 0, 0, 0, 7, 0, 0, 0, 2, 0x00, 0x00],
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
fn an_unkeyed_command_ends_in_an_absent_key() {
    for (unkeyed, mut body) in unkeyed_goldens() {
        body.push(0x00);
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
fn a_keyed_command_is_a_present_key_then_sixteen_bytes() {
    for (unkeyed, mut body) in unkeyed_goldens() {
        let keyed = keyed(&unkeyed);
        body.push(0x01);
        body.extend_from_slice(key().as_bytes());
        // The key lives inside field 2's length, so nothing past the
        // command field moves.
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
fn bytes_a_newer_build_appends_after_the_key_are_skipped() {
    for (unkeyed, body) in unkeyed_goldens() {
        let mut unkeyed_tail = body.clone();
        unkeyed_tail.extend_from_slice(&[0x00, 0xEE, 0xEE, 0xEE]);
        assert_eq!(
            decode(&command_frame(&unkeyed_tail)).unwrap(),
            command(unkeyed.clone())
        );
        let mut keyed_tail = body;
        keyed_tail.push(0x01);
        keyed_tail.extend_from_slice(key().as_bytes());
        keyed_tail.extend_from_slice(&[0xEE; 20]);
        assert_eq!(
            decode(&command_frame(&keyed_tail)).unwrap(),
            command(keyed(&unkeyed))
        );
    }
}

#[test]
fn a_zero_or_truncated_key_is_malformed() {
    for (_, mut body) in unkeyed_goldens() {
        body.push(0x01);
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
