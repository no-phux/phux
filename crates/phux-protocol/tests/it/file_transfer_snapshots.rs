//! Byte goldens for the file-transfer pull: `GET_FILE` and its `FILE_CHUNK`
//! reply. Kept apart from `frame_wire_snapshots.rs`, whose fixture table is
//! already at the authored-file size budget; the dump format is the same
//! `xxd` style so the two golden sets read alike.

#![allow(clippy::unwrap_used)]

use bytes::BytesMut;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, FileChunk, FrameKind};

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

fn fixtures() -> Vec<(&'static str, FrameKind)> {
    vec![
        // GET_FILE (tag 0x3a): a bounded read of a file on the named
        // satellite.
        (
            "snap_command_get_file",
            FrameKind::Command {
                request_id: 0x0000_0012,
                command: Command::GetFile {
                    host: Some("mini".to_owned()),
                    path: "/Users/me/Decks/talk.pptx".to_owned(),
                    offset: 0x0080_0000,
                    max_bytes: 0x0080_0000,
                },
            },
        ),
        // FILE_CHUNK (CommandValue 0x0a) on the chunk that reaches the end,
        // where the whole-file digest is present.
        (
            "snap_command_result_file_chunk_final",
            FrameKind::CommandResult {
                request_id: 0x0000_0012,
                result: CommandResult::OkWith(CommandValue::FileChunk(FileChunk {
                    size: 0x0080_0005,
                    data: b"tail!".to_vec(),
                    sha256: Some([0x7d; 32]),
                })),
            },
        ),
    ]
}

/// Every fixture's hex dump matches its committed golden byte-for-byte, and
/// decodes back to itself.
#[test]
fn file_transfer_frames_match_goldens() {
    for (name, frame) in fixtures() {
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        assert_eq!(FrameKind::decode(&buf).unwrap().0, frame, "{name}");
        insta::assert_snapshot!(name, hex_dump(&buf));
    }
}
