//! Byte goldens for `SERVICE_HTTP_OPEN`, `SERVICE_HTTP_READ`,
//! `SERVICE_HTTP_CLOSE`, and their `SERVICE_HTTP_HEAD` / `SERVICE_HTTP_CHUNK`
//! replies, in the `xxd` style of `file_transfer_snapshots.rs`.

#![allow(clippy::unwrap_used)]

use crate::common::dump_frame;
use phux_protocol::service_http::{ServiceHttpChunk, ServiceHttpHead, ServiceHttpRequest};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, FrameKind};

fn fixtures() -> Vec<(&'static str, FrameKind)> {
    vec![
        // SERVICE_HTTP_OPEN (tag 0x3d) to the serving server's OpenCode.
        (
            "snap_command_service_http_open",
            FrameKind::Command {
                request_id: 0x21,
                command: Command::ServiceHttpOpen {
                    request: Box::new(ServiceHttpRequest {
                        host: None,
                        service: "opencode".to_owned(),
                        method: "GET".to_owned(),
                        target: "/api/info".to_owned(),
                        headers: vec![("accept".to_owned(), "application/json".to_owned())],
                        body: Vec::new(),
                    }),
                },
            },
        ),
        // SERVICE_HTTP_READ (tag 0x3e) and SERVICE_HTTP_CLOSE (tag 0x3f).
        (
            "snap_command_service_http_read",
            FrameKind::Command {
                request_id: 0x22,
                command: Command::ServiceHttpRead {
                    exchange: 0x0102,
                    max_bytes: 0x0001_0000,
                },
            },
        ),
        (
            "snap_command_service_http_close",
            FrameKind::Command {
                request_id: 0x23,
                command: Command::ServiceHttpClose { exchange: 0x0102 },
            },
        ),
        // SERVICE_HTTP_HEAD (CommandValue 0x0c).
        (
            "snap_command_result_service_http_head",
            FrameKind::CommandResult {
                request_id: 0x21,
                result: CommandResult::OkWith(CommandValue::ServiceHttpHead(ServiceHttpHead {
                    exchange: 0x0102,
                    status: 200,
                    headers: vec![("content-type".to_owned(), "application/json".to_owned())],
                })),
            },
        ),
        // SERVICE_HTTP_CHUNK (CommandValue 0x0d), the last one.
        (
            "snap_command_result_service_http_chunk_end",
            FrameKind::CommandResult {
                request_id: 0x22,
                result: CommandResult::OkWith(CommandValue::ServiceHttpChunk(ServiceHttpChunk {
                    bytes: b"{}".to_vec(),
                    end: true,
                })),
            },
        ),
    ]
}

/// Every fixture's hex dump matches its committed golden byte-for-byte, and
/// decodes back to itself.
#[test]
fn service_http_frames_match_goldens() {
    for (name, frame) in fixtures() {
        insta::assert_snapshot!(name, dump_frame(&frame));
    }
}
