//! `FORWARD_*` commands and replies: decoder bounds. Their round trips at
//! the accepted bounds run over the wire-schema sample corpus
//! (`src/wire/decode/wire_schema/roundtrip_tests.rs`).

use super::{BytesMut, Command, CommandResult, CommandValue, DecodeError, FrameKind};
use phux_protocol::forward::{
    ForwardChunk, ForwardOp, MAX_AGENT_MESSAGE_BYTES, MAX_FORWARD_ADDR_BYTES,
    MAX_FORWARD_CHUNK_BYTES, agent_answer_type, sign_request_key,
};
use phux_protocol::wire::frame::AgentEvent;

const fn agent_request(id: u64, message: Vec<u8>) -> FrameKind {
    FrameKind::Event {
        terminal: None,
        event: AgentEvent::SshAgentRequest { id, message },
        stamp: None,
    }
}

fn decoded(frame: &FrameKind) -> Result<FrameKind, DecodeError> {
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    FrameKind::decode(&encoded).map(|(decoded, tail)| {
        assert!(tail.is_empty());
        decoded
    })
}

const fn command(op: ForwardOp) -> FrameKind {
    FrameKind::Command {
        request_id: 5,
        command: Command::Forward { op },
    }
}

const fn reply(value: CommandValue) -> FrameKind {
    FrameKind::CommandResult {
        request_id: 5,
        result: CommandResult::OkWith(value),
    }
}

fn max_read() -> u32 {
    u32::try_from(MAX_FORWARD_CHUNK_BYTES).unwrap()
}

#[test]
fn forward_decoders_refuse_what_exceeds_the_bounds() {
    for frame in [
        command(ForwardOp::Open {
            addr: "x".repeat(MAX_FORWARD_ADDR_BYTES + 1),
            port: 1,
        }),
        command(ForwardOp::Read {
            id: 1,
            max_bytes: max_read() + 1,
        }),
        command(ForwardOp::Write {
            id: 1,
            bytes: vec![0; MAX_FORWARD_CHUNK_BYTES + 1],
            fin: false,
        }),
        reply(CommandValue::ForwardChunk(ForwardChunk {
            bytes: vec![0; MAX_FORWARD_CHUNK_BYTES + 1],
            eof: true,
        })),
        command(ForwardOp::AgentReply {
            id: 1,
            message: vec![5; MAX_AGENT_MESSAGE_BYTES + 1],
        }),
    ] {
        assert_eq!(
            decoded(&frame).unwrap_err(),
            DecodeError::ForwardLimitExceeded
        );
    }
}

/// An over-long `agent_request` is opaque to the receiver, never a request
/// it could act on, and never a failed frame.
#[test]
fn an_over_long_agent_request_decodes_as_an_unknown_event() {
    let frame = agent_request(1, vec![13; MAX_AGENT_MESSAGE_BYTES + 1]);
    assert!(matches!(
        decoded(&frame).unwrap(),
        FrameKind::Event {
            event: AgentEvent::Unknown { tag: 0x21, .. },
            ..
        }
    ));
}

/// Only list and sign have an answer type, so only they are forwarded; a
/// sign request's key is its first string, and a short one has none.
#[test]
fn only_list_and_sign_requests_are_forwardable() {
    assert_eq!(agent_answer_type(&[11]), Some(12));
    assert_eq!(agent_answer_type(&[13, 0, 0, 0, 0]), Some(14));
    for other in 0..=u8::MAX {
        if other != 11 && other != 13 {
            assert_eq!(agent_answer_type(&[other]), None, "type {other}");
        }
    }
    assert_eq!(agent_answer_type(&[]), None);

    let sign = [13, 0, 0, 0, 3, b'k', b'e', b'y', 0, 0, 0, 0];
    assert_eq!(sign_request_key(&sign), Some(&b"key"[..]));
    assert_eq!(sign_request_key(&[13, 0, 0, 0, 9, b'k']), None);
    assert_eq!(sign_request_key(&[13, 0, 0]), None);
    assert_eq!(sign_request_key(&[11]), None);
}
