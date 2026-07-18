//! Integration tests for the ATTACH handler (`phux-byc.8`).
//!
//! Covers:
//! * `attach_returns_attached_and_pane_snapshot` — the happy path. A
//!   pre-seeded `default` session is on the server; the client sends
//!   `ATTACH { ByName("default") }` and receives `ATTACHED` followed by
//!   exactly one `TERMINAL_SNAPSHOT` (the seeded session has one pane).
//! * `attach_unknown_session_returns_error` — the failure path. The
//!   client sends `ATTACH { ByName("ghost") }` against a server with
//!   no session of that name and receives `ERROR { code:
//!   SessionNotFound }`. Unblocks `phux-byc.6.6` per the ticket.
//!
//! These tests drive the wire directly over a Unix socket, not through
//! `phux-client` — keeping them close to the protocol-level contract
//! so a future client refactor can't silently mask a regression.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use phux_protocol::wire::frame::{
    AttachTarget, ErrorCode, FrameKind, TYPE_ATTACHED, TYPE_ERROR, TYPE_TERMINAL_SNAPSHOT,
    ViewportInfo,
};
use phux_server::{ServerConfig, ServerError, ServerRuntime};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::sleep;

/// Same shape as the helper in `socket_lifecycle.rs`. Duplicated here
/// rather than shared via a module because integration tests are
/// independent binaries; sharing would require a `tests/common/`
/// pattern that complicates layout for ~20 lines of helper.
fn spawn_server_with_seeded_session(
    socket_path: PathBuf,
    pre_seeded: Option<&str>,
) -> (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>) {
    let (tx, rx) = oneshot::channel::<()>();
    let cfg = ServerConfig {
        socket_path,
        pre_seeded_session: pre_seeded.map(str::to_owned),
        seed_with_pty: false,
        seed_command: None,
        ..ServerConfig::with_default_socket()
    };
    let handle = tokio::task::spawn_local(async move {
        let server = ServerRuntime::new(cfg);
        server
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    (tx, handle)
}

async fn wait_for_socket(path: &Path, deadline: Duration) -> UnixStream {
    let start = Instant::now();
    let mut last_err: Option<std::io::Error> = None;
    while start.elapsed() < deadline {
        match UnixStream::connect(path).await {
            Ok(s) => return s,
            Err(e) => last_err = Some(e),
        }
        sleep(Duration::from_millis(5)).await;
    }
    panic!(
        "socket {} never became connectable: last_err={:?}",
        path.display(),
        last_err,
    );
}

/// Read one length-prefixed frame and reconstruct the framed bytes
/// (header + body). Returns the framed bytes so callers can hand them
/// to `FrameKind::decode`.
async fn read_one_frame_bytes(stream: &mut UnixStream) -> Vec<u8> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await.unwrap();
    let body_len = u32::from_be_bytes(header) as usize;
    let mut body = vec![0u8; body_len];
    stream.read_exact(&mut body).await.unwrap();
    let mut framed = Vec::with_capacity(4 + body_len);
    framed.extend_from_slice(&header);
    framed.extend_from_slice(&body);
    framed
}

#[allow(
    dead_code,
    reason = "kept for future scenarios that don't peek the type byte"
)]
async fn read_frame(stream: &mut UnixStream) -> FrameKind {
    let framed = read_one_frame_bytes(stream).await;
    let (frame, rest) = FrameKind::decode(&framed).expect("decode frame");
    assert!(rest.is_empty(), "decoder did not consume entire frame");
    frame
}

/// Peek the type byte of the next frame without consuming the body.
/// Useful for assertions that drive on `body[0]` directly.
async fn read_typed_frame(stream: &mut UnixStream) -> (u8, FrameKind) {
    let framed = read_one_frame_bytes(stream).await;
    // body starts after 4-byte length header; type byte is body[0].
    let type_byte = framed[4];
    let (frame, _rest) = FrameKind::decode(&framed).expect("decode frame");
    (type_byte, frame)
}

fn encode_frame(frame: &FrameKind) -> BytesMut {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf
}

fn attach_by_name(name: &str) -> FrameKind {
    FrameKind::Attach {
        target: AttachTarget::ByName(name.to_owned()),
        viewport: ViewportInfo::new(80, 24),
        request_scrollback: false,
        scrollback_limit_lines: 0,
    }
}

fn run_local<F>(fut: F)
where
    F: std::future::Future<Output = ()>,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, fut);
}

/// Happy path: `ATTACH { ByName("default") }` → `ATTACHED` + `TERMINAL_SNAPSHOT`.
#[test]
fn attach_returns_attached_and_pane_snapshot() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seeded_session(socket_path.clone(), Some("default"));

        let mut stream = wait_for_socket(&socket_path, Duration::from_secs(2)).await;
        stream
            .write_all(&encode_frame(&attach_by_name("default")))
            .await
            .unwrap();
        stream.flush().await.unwrap();

        // Frame 1: ATTACHED with a SessionSnapshot describing the
        // pre-seeded session+window+pane.
        let (type_byte, frame) = read_typed_frame(&mut stream).await;
        assert_eq!(type_byte, TYPE_ATTACHED, "first frame should be ATTACHED");
        match &frame {
            FrameKind::Attached {
                snapshot,
                initial_client_id,
            } => {
                assert_eq!(snapshot.sessions.len(), 1, "exactly one session");
                assert_eq!(snapshot.sessions[0].name, "default");
                assert_eq!(snapshot.windows.len(), 1, "exactly one window");
                assert_eq!(snapshot.panes.len(), 1, "exactly one pane");
                assert!(
                    initial_client_id.get() >= 1,
                    "client id should be allocated (got {})",
                    initial_client_id.get(),
                );
            }
            other => panic!("expected Attached, got {other:?}"),
        }

        // Frame 2: TERMINAL_SNAPSHOT for the session's one pane.
        let (type_byte, frame) = read_typed_frame(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_TERMINAL_SNAPSHOT,
            "second frame should be TERMINAL_SNAPSHOT",
        );
        match frame {
            FrameKind::TerminalSnapshot {
                terminal_id: _,
                cols,
                rows,
                vt_replay_bytes,
                scrollback_bytes,
            } => {
                // Pane was created with `TerminalActor::new(80, 24)` per
                // the runtime's seed path.
                assert_eq!(cols, 80);
                assert_eq!(rows, 24);
                // Blank pane still has the reset preamble +
                // CUP-home — never empty.
                assert!(
                    vt_replay_bytes.starts_with(b"\x1b[!p\x1b[2J\x1b[H"),
                    "snapshot bytes should start with reset preamble",
                );
                assert!(
                    scrollback_bytes.is_none(),
                    "byc.8 never sends scrollback (deferred to byc.5)",
                );
            }
            other => panic!("expected TerminalSnapshot, got {other:?}"),
        }

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}

/// Failure path: ATTACH against an unknown session yields ERROR
/// with `SessionNotFound`. Per the byc.8 ticket this unblocks
/// `phux-byc.6.6` (`attach_unknown_session_returns_error`).
#[test]
fn attach_unknown_session_returns_error() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // No pre-seeded session — every ATTACH must fail.
        let (shutdown_tx, server_handle) =
            spawn_server_with_seeded_session(socket_path.clone(), None);

        let mut stream = wait_for_socket(&socket_path, Duration::from_secs(2)).await;
        stream
            .write_all(&encode_frame(&attach_by_name("ghost")))
            .await
            .unwrap();
        stream.flush().await.unwrap();

        let (type_byte, frame) = read_typed_frame(&mut stream).await;
        assert_eq!(type_byte, TYPE_ERROR, "expected ERROR frame");
        match frame {
            FrameKind::Error {
                request_id,
                code,
                message,
            } => {
                assert!(
                    request_id.is_none(),
                    "ATTACH errors are not command-correlated (no request_id)",
                );
                assert_eq!(code, ErrorCode::SessionNotFound);
                assert!(
                    message.contains("ghost"),
                    "error message should mention the requested session, got: {message:?}",
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}

/// PING still works in the new world (regression guard for the `LocalSet`
/// flip). Same logic as `socket_lifecycle::lifecycle_ping_pong` but
/// targeted at the attach test binary so the `LocalSet` wrap-up doesn't
/// drift between binaries.
#[test]
fn ping_pong_still_works_after_localset_flip() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seeded_session(socket_path.clone(), None);

        let mut stream = wait_for_socket(&socket_path, Duration::from_secs(2)).await;
        let ping = encode_frame(&FrameKind::Ping { nonce: 0xABCD });
        stream.write_all(&ping).await.unwrap();
        stream.flush().await.unwrap();
        let (_type_byte, frame) = read_typed_frame(&mut stream).await;
        assert_eq!(frame, FrameKind::Pong { nonce: 0xABCD });

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}
