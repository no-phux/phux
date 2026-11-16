//! Byte goldens for `FORWARD_OPEN`, `FORWARD_READ`, `FORWARD_WRITE`,
//! `FORWARD_CLOSE`, and their `FORWARD_OPENED` / `FORWARD_CHUNK` replies, in
//! the `xxd` style of `file_transfer_snapshots.rs`.

#![allow(clippy::unwrap_used)]

use crate::common::dump_frame;
use phux_protocol::forward::{ForwardChunk, ForwardOp};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, FrameKind};

const fn command(request_id: u32, op: ForwardOp) -> FrameKind {
    FrameKind::Command {
        request_id,
        command: Command::Forward { op },
    }
}

const fn reply(request_id: u32, value: CommandValue) -> FrameKind {
    FrameKind::CommandResult {
        request_id,
        result: CommandResult::OkWith(value),
    }
}

fn fixtures() -> Vec<(&'static str, FrameKind)> {
    vec![
        // FORWARD_OPEN (tag 0x44).
        (
            "snap_command_forward_open",
            command(
                0x21,
                ForwardOp::Open {
                    addr: "localhost".to_owned(),
                    port: 3000,
                },
            ),
        ),
        // FORWARD_READ (tag 0x45).
        (
            "snap_command_forward_read",
            command(
                0x22,
                ForwardOp::Read {
                    id: 0x0102,
                    max_bytes: 0x0004_0000,
                },
            ),
        ),
        // FORWARD_WRITE (tag 0x46), the last one.
        (
            "snap_command_forward_write_fin",
            command(
                0x23,
                ForwardOp::Write {
                    id: 0x0102,
                    bytes: b"ping".to_vec(),
                    fin: true,
                },
            ),
        ),
        // FORWARD_CLOSE (tag 0x47).
        (
            "snap_command_forward_close",
            command(0x24, ForwardOp::Close { id: 0x0102 }),
        ),
        // AGENT_OFFER (tag 0x4f): no body.
        (
            "snap_command_agent_offer",
            command(0x25, ForwardOp::AgentOffer),
        ),
        // AGENT_REPLY (tag 0x50): SSH_AGENT_FAILURE for request 0x0102.
        (
            "snap_command_agent_reply",
            command(
                0x26,
                ForwardOp::AgentReply {
                    id: 0x0102,
                    message: vec![5],
                },
            ),
        ),
        // agent_request (event tag 0x21): SSH_AGENTC_REQUEST_IDENTITIES.
        (
            "snap_event_agent_request",
            FrameKind::Event {
                terminal: None,
                event: phux_protocol::wire::frame::AgentEvent::SshAgentRequest {
                    id: 0x0102,
                    message: vec![11],
                },
                stamp: None,
            },
        ),
        // FORWARD_OPENED (CommandValue 0x0f).
        (
            "snap_command_result_forward_opened",
            reply(0x21, CommandValue::ForwardOpened(0x0102)),
        ),
        // FORWARD_CHUNK (CommandValue 0x10), the last one.
        (
            "snap_command_result_forward_chunk_eof",
            reply(
                0x22,
                CommandValue::ForwardChunk(ForwardChunk {
                    bytes: b"pong".to_vec(),
                    eof: true,
                }),
            ),
        ),
    ]
}

/// Every fixture's hex dump matches its committed golden byte-for-byte, and
/// decodes back to itself.
#[test]
fn forward_frames_match_goldens() {
    for (name, frame) in fixtures() {
        insta::assert_snapshot!(name, dump_frame(&frame));
    }
}
