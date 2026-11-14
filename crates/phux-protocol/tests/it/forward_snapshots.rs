//! Byte goldens for `FORWARD_OPEN`, `FORWARD_READ`, `FORWARD_WRITE`,
//! `FORWARD_CLOSE`, and their `FORWARD_OPENED` / `FORWARD_CHUNK` replies, in
//! the `xxd` style of `file_transfer_snapshots.rs`.

#![allow(clippy::unwrap_used)]

use bytes::BytesMut;
use phux_protocol::forward::{ForwardChunk, ForwardOp};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, FrameKind};

fn hex_dump(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    for (chunk_idx, chunk) in bytes.chunks(16).enumerate() {
        let _ = write!(out, "{:08x} |", chunk_idx * 16);
        for (i, b) in chunk.iter().enumerate() {
            if i == 8 {
                out.push(' ');
            }
            let _ = write!(out, " {b:02x}");
        }
        for i in 0..16 - chunk.len() {
            if chunk.len() + i == 8 {
                out.push(' ');
            }
            out.push_str("   ");
        }
        out.push_str(" |");
        for b in chunk {
            out.push(if (0x20..=0x7e).contains(b) {
                *b as char
            } else {
                '.'
            });
        }
        out.push('\n');
    }
    out
}

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
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        assert_eq!(FrameKind::decode(&buf).unwrap().0, frame, "{name}");
        insta::assert_snapshot!(name, hex_dump(&buf));
    }
}
