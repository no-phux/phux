//! Byte goldens for `SERVICE_HTTP_OPEN`, `SERVICE_HTTP_READ`,
//! `SERVICE_HTTP_CLOSE`, and their `SERVICE_HTTP_HEAD` / `SERVICE_HTTP_CHUNK`
//! replies, in the `xxd` style of `file_transfer_snapshots.rs`.

#![allow(clippy::unwrap_used)]

use bytes::BytesMut;
use phux_protocol::service_http::{ServiceHttpChunk, ServiceHttpHead, ServiceHttpRequest};
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
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        assert_eq!(FrameKind::decode(&buf).unwrap().0, frame, "{name}");
        insta::assert_snapshot!(name, hex_dump(&buf));
    }
}
