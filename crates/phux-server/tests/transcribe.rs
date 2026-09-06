//! `TRANSCRIBE` over the wire: a finished `PUT_FILE` upload is handed to the
//! configured transcriber and the transcript lands in the pane as a paste.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet, ServerFeature};
use phux_protocol::ids::{FileUploadId, TerminalId};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, FrameKind, TYPE_ATTACH_READY,
    TYPE_COMMAND_RESULT, TYPE_HELLO_OK, TYPE_TERMINAL_OUTPUT,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame, spawn_server_with,
    wait_for_raw_socket,
};

const SESSION: &str = "voice";
const HEARD: &str = "hello from voice";

async fn connect(path: &std::path::Path) -> UnixStream {
    let mut stream = wait_for_raw_socket(path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &FrameKind::Hello {
            client_name: "transcribe-test".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new()
                .with_color_support(ColorSupport::TrueColor)
                .with_layers(LayerSet::all()),
        },
    )
    .await;
    let (type_byte, frame) = recv_typed(&mut stream).await;
    assert_eq!(type_byte, TYPE_HELLO_OK);
    let FrameKind::HelloOk { server_caps, .. } = frame else {
        panic!("expected HELLO_OK, got {frame:?}");
    };
    assert!(
        server_caps.features.contains(ServerFeature::Transcribe),
        "server must advertise TRANSCRIBE"
    );
    stream
}

/// Attach and return the seed pane's id from the `ATTACHED` snapshot.
async fn attach(stream: &mut UnixStream) -> TerminalId {
    send_frame(stream, &attach_by_name(SESSION)).await;
    let mut pane = None;
    loop {
        let (type_byte, frame) = recv_typed(stream).await;
        if let FrameKind::Attached { snapshot, .. } = &frame {
            pane = snapshot.panes.first().map(|p| p.id.clone());
        }
        if type_byte == TYPE_ATTACH_READY {
            break;
        }
    }
    pane.expect("ATTACHED carried the seed pane")
}

async fn command_result(stream: &mut UnixStream, request_id: u32) -> CommandResult {
    loop {
        let (type_byte, frame) = recv_typed(stream).await;
        if type_byte != TYPE_COMMAND_RESULT {
            continue;
        }
        if let FrameKind::CommandResult {
            request_id: got,
            result,
        } = frame
            && got == request_id
        {
            return result;
        }
    }
}

async fn upload(stream: &mut UnixStream, pane: &TerminalId, bytes: &[u8]) -> FileUploadId {
    let upload_id = FileUploadId::new([0x5a; 16]).expect("non-zero");
    send_frame(
        stream,
        &FrameKind::Command {
            request_id: 10,
            command: Command::PutFile {
                upload_id,
                terminal_id: pane.clone(),
                extension: "wav".to_owned(),
                offset: 0,
                data: bytes.to_vec(),
                final_chunk: true,
                sha256: Some(Sha256::digest(bytes).into()),
            },
        },
    )
    .await;
    match command_result(stream, 10).await {
        CommandResult::OkWith(CommandValue::FileUpload(ack)) => {
            assert!(ack.path.is_some(), "final chunk must land the file");
        }
        other => panic!("PUT_FILE failed: {other:?}"),
    }
    upload_id
}

fn cat_seed() -> portable_pty::CommandBuilder {
    portable_pty::CommandBuilder::new("cat")
}

/// A transcriber that proves it saw the clip (non-empty file at `{path}`)
/// and answers with a fixed phrase.
fn fake_transcriber() -> Vec<String> {
    vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        format!("test -s \"$1\" && printf '%s\\n' '{HEARD}'"),
        "sh".to_owned(),
        "{path}".to_owned(),
    ]
}

#[test]
fn transcribe_pastes_the_transcript_into_the_pane_and_returns_it() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        // Route the upload sandbox into the test's temp dir.
        // SAFETY-free: tests in this binary run on one thread per process.
        unsafe { std::env::set_var("PHUX_UPLOAD_DIR", tmp.path().join("uploads")) };
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) =
            spawn_server_with(socket_path.clone(), Some(SESSION), |cfg| {
                cfg.seed_with_pty = true;
                cfg.seed_command = Some(cat_seed());
                cfg.voice.transcriber = Some(fake_transcriber());
                cfg.voice.timeout_secs = Some(10);
            });

        let mut stream = connect(&socket_path).await;
        let pane = attach(&mut stream).await;
        let upload_id = upload(&mut stream, &pane, b"RIFF....WAVEfmt fake audio").await;

        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 11,
                command: Command::Transcribe {
                    upload_id,
                    terminal_id: pane.clone(),
                },
            },
        )
        .await;
        // The reply and the echoed paste can arrive in either order.
        let mut reply: Option<String> = None;
        let mut echoed = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            let (type_byte, frame) = recv_typed(&mut stream).await;
            match frame {
                FrameKind::CommandResult {
                    request_id: 11,
                    result,
                } => match result {
                    CommandResult::OkWith(CommandValue::Json(json)) => reply = Some(json),
                    other => panic!("TRANSCRIBE failed: {other:?}"),
                },
                FrameKind::TerminalOutput { bytes, .. } if type_byte == TYPE_TERMINAL_OUTPUT => {
                    echoed.extend_from_slice(&bytes);
                }
                _ => {}
            }
            let echoed_text = String::from_utf8_lossy(&echoed);
            if reply.is_some() && echoed_text.contains(HEARD) {
                break;
            }
        }
        let json = reply.expect("TRANSCRIBE replied");
        let value: serde_json::Value = serde_json::from_str(&json).expect("reply is JSON");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["text"], HEARD);
        assert_eq!(value["pasted"], true);
        assert!(
            String::from_utf8_lossy(&echoed).contains(HEARD),
            "cat should echo the pasted transcript; saw {:?}",
            String::from_utf8_lossy(&echoed)
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}

#[test]
fn transcribe_without_a_transcriber_is_refused_with_a_remedy() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) =
            spawn_server_with(socket_path.clone(), Some(SESSION), |_| {});
        let mut stream = connect(&socket_path).await;
        let pane = attach(&mut stream).await;
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 12,
                command: Command::Transcribe {
                    upload_id: FileUploadId::new([1; 16]).expect("non-zero"),
                    terminal_id: pane,
                },
            },
        )
        .await;
        match command_result(&mut stream, 12).await {
            CommandResult::Error { code, message } => {
                assert_eq!(code, ErrorCode::InvalidCommand);
                assert!(
                    message.contains("[voice] transcriber"),
                    "remedy missing: {message}"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}
