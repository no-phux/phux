//! `FORWARD_*` commands and replies: round trips and decoder bounds.

use super::{BytesMut, Command, CommandResult, CommandValue, DecodeError, FrameKind};
use phux_protocol::forward::{
    ForwardChunk, ForwardOp, MAX_FORWARD_ADDR_BYTES, MAX_FORWARD_CHUNK_BYTES,
};

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
fn forward_commands_and_replies_round_trip() {
    let frames = [
        command(ForwardOp::Open {
            addr: "::1".to_owned(),
            port: u16::MAX,
        }),
        command(ForwardOp::Open {
            addr: "x".repeat(MAX_FORWARD_ADDR_BYTES),
            port: 1,
        }),
        command(ForwardOp::Read {
            id: u64::MAX,
            max_bytes: max_read(),
        }),
        command(ForwardOp::Write {
            id: 9,
            bytes: vec![0xa5; MAX_FORWARD_CHUNK_BYTES],
            fin: false,
        }),
        command(ForwardOp::Write {
            id: 9,
            bytes: Vec::new(),
            fin: true,
        }),
        command(ForwardOp::Close { id: 9 }),
        reply(CommandValue::ForwardOpened(u64::MAX)),
        reply(CommandValue::ForwardChunk(ForwardChunk {
            bytes: vec![0x5a; MAX_FORWARD_CHUNK_BYTES],
            eof: false,
        })),
        reply(CommandValue::ForwardChunk(ForwardChunk {
            bytes: Vec::new(),
            eof: true,
        })),
    ];
    for frame in frames {
        assert_eq!(decoded(&frame).unwrap(), frame);
    }
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
    ] {
        assert_eq!(
            decoded(&frame).unwrap_err(),
            DecodeError::ForwardLimitExceeded
        );
    }
}
