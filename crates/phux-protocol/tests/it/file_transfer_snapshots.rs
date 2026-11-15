//! Byte goldens for the file-transfer pull: `GET_FILE` and its `FILE_CHUNK`
//! reply. Kept apart from `frame_wire_snapshots.rs`, whose fixture table is
//! already at the authored-file size budget; the dump format is the same
//! `xxd` style so the two golden sets read alike.

#![allow(clippy::unwrap_used)]

use crate::common::dump_frame;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, FileChunk, FrameKind};

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
        insta::assert_snapshot!(name, dump_frame(&frame));
    }
}
