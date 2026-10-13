//! Upload helper acceptance over disposable UDS sockets and upload roots.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]
#![allow(clippy::future_not_send, reason = "ServerRuntime owns LocalSet actors")]

use std::path::Path;
use std::time::Duration;

use bytes::BytesMut;
use phux_client::upload::{
    UploadError, UploadOutcome, UploadReceipt, infer_extension, load_file, upload_bytes,
    upload_file,
};
use phux_client_runtime::runtime::Target;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::ResourceId;
use phux_protocol::caps::{
    BootstrapCapabilities, ServerCapabilities, ServerFeature, ServerFeatureSet,
    select_bootstrap_profile,
};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, FileUploadAck, FrameKind, MAX_FILE_UPLOAD_SIZE,
};
use phux_protocol::wire::framing::{self, LENGTH_PREFIX_LEN};
use phux_server::{ServerConfig, ServerEnv, ServerRuntime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};

const BUDGET: Duration = Duration::from_secs(15);

async fn recv(stream: &mut UnixStream) -> Option<FrameKind> {
    let mut header = [0; LENGTH_PREFIX_LEN];
    match stream.read_exact(&mut header).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return None,
        Err(error) => panic!("read frame: {error}"),
    }
    let mut encoded = framing::frame_buffer(header).unwrap();
    stream
        .read_exact(&mut encoded[LENGTH_PREFIX_LEN..])
        .await
        .unwrap();
    let (frame, tail) = FrameKind::decode(&encoded).unwrap();
    assert!(tail.is_empty());
    Some(frame)
}

async fn send(stream: &mut UnixStream, frame: FrameKind) {
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    // The one-shot helper closes immediately after a terminal receipt, even
    // if a previously queued browse reply is still being written.
    if let Err(error) = stream.write_all(&encoded).await {
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
        ));
    }
}

#[derive(Clone, Copy)]
enum Answer {
    Unsupported,
    Refused,
    Malformed,
    Silent,
    Completed,
}

async fn scripted(listener: UnixListener, answer: Answer) -> Vec<FrameKind> {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut seen = Vec::new();
    while let Some(frame) = recv(&mut stream).await {
        seen.push(frame.clone());
        match frame {
            FrameKind::Hello { client_caps, .. } => {
                let (selected_profile, bootstrap_limits) =
                    select_bootstrap_profile(&client_caps, &BootstrapCapabilities::new()).unwrap();
                let features = if matches!(answer, Answer::Unsupported) {
                    ServerFeatureSet::new()
                } else {
                    ServerFeatureSet::with(&[ServerFeature::FileUpload])
                };
                send(
                    &mut stream,
                    FrameKind::HelloOk {
                        protocol_major: PROTOCOL_VERSION.major,
                        protocol_minor: PROTOCOL_VERSION.minor,
                        protocol_patch: PROTOCOL_VERSION.patch,
                        server_caps: ServerCapabilities::new().with_features(features),
                        server_id: b"upload-test".to_vec(),
                        selected_profile,
                        bootstrap_limits,
                    },
                )
                .await;
            }
            FrameKind::Command {
                request_id,
                command:
                    Command::PutFile {
                        data,
                        offset,
                        final_chunk,
                        ..
                    },
            } => {
                if matches!(answer, Answer::Silent) {
                    continue;
                }
                let result = match answer {
                    Answer::Refused => CommandResult::Error {
                        code: ErrorCode::InvalidCommand,
                        message: "upload refused by fixture".to_owned(),
                    },
                    Answer::Malformed => {
                        CommandResult::OkWith(CommandValue::FileUpload(FileUploadAck {
                            next_offset: u64::MAX,
                            path: None,
                        }))
                    }
                    Answer::Completed => {
                        CommandResult::OkWith(CommandValue::FileUpload(FileUploadAck {
                            next_offset: offset + u64::try_from(data.len()).unwrap(),
                            path: final_chunk.then(|| "/owning-host/retained.bin".to_owned()),
                        }))
                    }
                    Answer::Unsupported | Answer::Silent => panic!("unexpected upload"),
                };
                send(&mut stream, FrameKind::CommandResult { request_id, result }).await;
            }
            FrameKind::Command { request_id, .. } => {
                // Browse GET_STATE receives no topology in this fault fixture.
                send(
                    &mut stream,
                    FrameKind::CommandResult {
                        request_id,
                        result: CommandResult::Ok,
                    },
                )
                .await;
            }
            _ => {}
        }
    }
    seen
}

fn assert_browse_only(seen: &[FrameKind]) {
    assert!(
        seen.iter()
            .any(|frame| matches!(frame, FrameKind::Hello { .. }))
    );
    for frame in seen {
        assert!(
            matches!(
                frame,
                FrameKind::Hello { .. }
                    | FrameKind::Ping { .. }
                    | FrameKind::SubscribeEvents { .. }
                    | FrameKind::SubscribeMetadata { .. }
                    | FrameKind::Command {
                        command: Command::GetState { .. } | Command::PutFile { .. },
                        ..
                    }
            ),
            "unexpected side effect: {frame:?}"
        );
    }
}

async fn exercise(
    answer: Answer,
    budget: Duration,
    terminal: ResourceId,
    extension: &str,
) -> (UploadReceipt, Vec<FrameKind>) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fixture.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(scripted(listener, answer));
    let receipt = upload_bytes(
        Target::uds(socket),
        terminal,
        extension.to_owned(),
        b"test bytes".to_vec(),
        budget,
    )
    .await
    .unwrap();
    let seen = timeout(BUDGET, server).await.unwrap().unwrap();
    assert_browse_only(&seen);
    (receipt, seen)
}

#[tokio::test]
async fn upload_outcomes_preserve_wire_detail_and_null_path() {
    for (answer, expected) in [
        (Answer::Unsupported, UploadOutcome::Refused),
        (Answer::Refused, UploadOutcome::Refused),
        (Answer::Malformed, UploadOutcome::Unknown),
    ] {
        let (receipt, seen) = exercise(answer, BUDGET, ResourceId::local(1), "bin").await;
        assert_eq!(receipt.outcome, expected);
        assert_eq!(receipt.path, None);
        assert!(receipt.transfer_id > 0);
        assert!(!receipt.message.is_empty());
        if matches!(answer, Answer::Unsupported) {
            assert!(!seen.iter().any(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::PutFile { .. },
                    ..
                }
            )));
        }
        if matches!(answer, Answer::Refused) {
            assert_eq!(receipt.code, Some(ErrorCode::InvalidCommand.as_wire()));
            assert_eq!(receipt.message, "upload refused by fixture");
        }
        let json = serde_json::to_value(&receipt).unwrap();
        assert!(json["path"].is_null());
        assert_eq!(
            json["outcome"],
            if expected == UploadOutcome::Unknown {
                "unknown"
            } else {
                "refused"
            }
        );
        assert_eq!(
            serde_json::from_value::<UploadReceipt>(json).unwrap(),
            receipt
        );
    }
}

#[tokio::test]
async fn upload_deadline_distinguishes_attempted_from_unattempted() {
    let (receipt, _) = exercise(
        Answer::Silent,
        Duration::from_millis(300),
        ResourceId::local(1),
        "bin",
    )
    .await;
    assert_eq!(receipt.outcome, UploadOutcome::Unknown);
    assert!(receipt.message.contains("deadline"));

    let dir = tempfile::tempdir().unwrap();
    let receipt = upload_bytes(
        Target::uds(dir.path().join("absent.sock")),
        ResourceId::local(1),
        "bin".to_owned(),
        vec![1],
        Duration::from_millis(50),
    )
    .await
    .unwrap();
    assert_eq!(receipt.outcome, UploadOutcome::Refused);
    assert!(receipt.message.contains("deadline"));
    assert!(matches!(
        upload_bytes(
            Target::uds(dir.path().join("absent.sock")),
            ResourceId::local(1),
            "bin".to_owned(),
            vec![1],
            Duration::ZERO
        )
        .await,
        Err(UploadError::Deadline)
    ));
}

#[tokio::test]
async fn upload_preserves_satellite_owner_and_runtime_extension_validation() {
    let terminal = ResourceId::satellite("upload-satellite", 42);
    let (receipt, seen) = exercise(Answer::Completed, BUDGET, terminal.clone(), "bin").await;
    assert_eq!(receipt.terminal_id, terminal);
    assert_eq!(receipt.outcome, UploadOutcome::Completed);
    assert_eq!(receipt.path.as_deref(), Some("/owning-host/retained.bin"));
    let json = serde_json::to_value(&receipt).unwrap();
    assert_eq!(json["outcome"], "completed");
    assert_eq!(json["terminal_id"]["Satellite"]["host"], "upload-satellite");
    assert_eq!(
        serde_json::from_value::<UploadReceipt>(json).unwrap(),
        receipt
    );
    assert!(seen.iter().any(|frame| matches!(frame, FrameKind::Command { command: Command::PutFile { terminal_id, .. }, .. } if terminal_id == &terminal)));
    let dir = tempfile::tempdir().unwrap();
    let receipt = upload_bytes(
        Target::uds(dir.path().join("absent.sock")),
        terminal,
        "bad.ext".to_owned(),
        vec![1],
        BUDGET,
    )
    .await
    .unwrap();
    assert_eq!(receipt.outcome, UploadOutcome::Refused);
    assert_eq!(receipt.code, Some(ErrorCode::InvalidCommand.as_wire()));
    assert!(receipt.message.contains("extension"));
}

#[tokio::test]
async fn upload_cancellation_closes_socket() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("cancel.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let task = tokio::spawn(upload_bytes(
        Target::uds(socket),
        ResourceId::local(1),
        "bin".to_owned(),
        vec![1],
        BUDGET,
    ));
    let (mut stream, _) = timeout(BUDGET, listener.accept()).await.unwrap().unwrap();
    assert!(matches!(
        recv(&mut stream).await,
        Some(FrameKind::Hello { .. })
    ));
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(timeout(BUDGET, recv(&mut stream)).await.unwrap().is_none());
}

#[tokio::test]
async fn local_loader_and_extension_bounds() {
    for (path, extension) in [
        ("image.PNG", "PNG"),
        ("archive.tar.gz", "gz"),
        ("no-extension", "bin"),
        ("a.bad-ext", "bin"),
        ("a.é", "bin"),
        ("a.12345678901234567", "bin"),
    ] {
        assert_eq!(infer_extension(Path::new(path)), extension);
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.bin");
    std::fs::write(&path, b"local bytes").unwrap();
    assert_eq!(load_file(path.clone()).await.unwrap(), b"local bytes");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(MAX_FILE_UPLOAD_SIZE + 1)
        .unwrap();
    assert!(matches!(load_file(path).await, Err(UploadError::TooLarge)));
    assert!(matches!(
        load_file(dir.path().to_path_buf()).await,
        Err(UploadError::NotRegularFile)
    ));
    assert!(matches!(
        load_file(dir.path().join("missing")).await,
        Err(UploadError::Io(_))
    ));
    for budget in [Duration::ZERO, Duration::MAX] {
        assert!(matches!(
            upload_file(
                Target::uds(dir.path().join("absent.sock")),
                ResourceId::local(1),
                dir.path().join("missing"),
                None,
                budget
            )
            .await,
            Err(UploadError::Deadline)
        ));
    }
    let exact_limit = dir.path().join("limit.bin");
    std::fs::File::create(&exact_limit)
        .unwrap()
        .set_len(MAX_FILE_UPLOAD_SIZE)
        .unwrap();
    assert_eq!(
        u64::try_from(load_file(exact_limit).await.unwrap().len()).unwrap(),
        MAX_FILE_UPLOAD_SIZE
    );
}

#[tokio::test(flavor = "current_thread")]
async fn real_uds_upload_retains_multiple_chunks_and_empty_file() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("server.sock");
            let root = dir.path().join("uploads");
            let (shutdown, stop) = oneshot::channel();
            let config = ServerConfig {
                socket_path: socket.clone(),
                pre_seeded_session: Some("upload-test".to_owned()),
                seed_with_pty: false,
                env: ServerEnv {
                    upload_dir: Some(root.clone()),
                    ..ServerEnv::default()
                },
                ..ServerConfig::with_default_socket()
            };
            let server = tokio::task::spawn_local(async move {
                ServerRuntime::new(config)
                    .run_async(async {
                        let _ = stop.await;
                    })
                    .await
            });
            timeout(BUDGET, async {
                while phux_client::state::get_state(&socket).await.is_err() {
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let source = dir.path().join("multiple.dat");
            let data: Vec<u8> = (0..2 * 1024 * 1024 + 37)
                .map(|i| u8::try_from(i % 251).unwrap())
                .collect();
            std::fs::write(&source, &data).unwrap();
            let receipt = upload_file(
                Target::uds(&socket),
                ResourceId::local(1),
                source,
                None,
                BUDGET,
            )
            .await
            .unwrap();
            assert_eq!(receipt.outcome, UploadOutcome::Completed, "{receipt:?}");
            let path = Path::new(receipt.path.as_deref().unwrap());
            assert_eq!(path.parent(), Some(root.as_path()));
            assert_eq!(path.extension().unwrap(), "dat");
            assert_eq!(std::fs::read(path).unwrap(), data);
            let empty = upload_bytes(
                Target::uds(&socket),
                ResourceId::local(1),
                "bin".to_owned(),
                vec![],
                BUDGET,
            )
            .await
            .unwrap();
            assert_eq!(empty.outcome, UploadOutcome::Completed);
            assert_eq!(std::fs::read(empty.path.unwrap()).unwrap(), b"");
            let invalid = upload_bytes(
                Target::uds(&socket),
                ResourceId::local(99999),
                "bin".to_owned(),
                vec![1],
                BUDGET,
            )
            .await
            .unwrap();
            assert_eq!(invalid.outcome, UploadOutcome::Refused);
            shutdown.send(()).unwrap();
            timeout(BUDGET, server).await.unwrap().unwrap().unwrap();
        })
        .await;
}
