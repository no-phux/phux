//! Integration tests for the `phux-server` UDS listener (phux-byc.3).
//!
//! Covers:
//! * `lifecycle_ping_pong` — bind, accept, PING/PONG round-trip, clean
//!   shutdown unlinks the socket.
//! * `lifecycle_stale_socket` — a leftover regular file at the socket path
//!   is removed and the bind succeeds.
//! * `lifecycle_busy_socket` — a second server at the same path is rejected
//!   with `ServerError::SocketBusy`.
//! * `lifecycle_partial_frame_disconnect` — a client that sends a partial
//!   length-prefix and drops the stream doesn't crash the server; another
//!   client can still PING/PONG afterwards.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use phux_protocol::wire::frame::FrameKind;
use phux_server::{ServerConfig, ServerError, ServerRuntime};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::sleep;

/// Spawn a server task and return its shutdown channel + `JoinHandle`.
///
/// Per ADR-0014, `ServerRuntime::run_async` returns a `!Send` future
/// because the per-pane `TerminalActor` it spawns owns a libghostty
/// `Terminal` (which is `!Send`). Tests therefore call this helper
/// inside a `tokio::task::LocalSet::run_until` and use
/// `tokio::task::spawn_local` instead of `tokio::spawn`.
fn spawn_server(
    socket_path: PathBuf,
) -> (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>) {
    let (tx, rx) = oneshot::channel::<()>();
    let cfg = ServerConfig {
        socket_path,
        pre_seeded_session: None,
        seed_with_pty: false,
        seed_command: None,
        ..ServerConfig::with_default_socket()
    };
    let handle = tokio::task::spawn_local(async move {
        let server = ServerRuntime::new(cfg);
        server
            .run_async(async move {
                // If the sender is dropped, treat that as a shutdown too.
                let _ = rx.await;
            })
            .await
    });
    (tx, handle)
}

/// Poll until a `UnixStream` can connect to `path` (with a deadline). Just
/// checking `path.exists()` isn't sufficient because a stale regular file
/// at the same path also makes `exists()` true while the server is still
/// removing it; the only race-free signal is "connect actually succeeds".
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

/// Encode a PING frame using the protocol crate (the canonical encoder).
fn encode_ping(nonce: u64) -> BytesMut {
    let mut buf = BytesMut::new();
    FrameKind::Ping { nonce }.encode(&mut buf);
    buf
}

/// Read and decode one length-prefixed frame from the stream.
async fn read_one_frame(stream: &mut UnixStream) -> FrameKind {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await.unwrap();
    let body_len = u32::from_be_bytes(header) as usize;
    let mut body = vec![0u8; body_len];
    stream.read_exact(&mut body).await.unwrap();
    let mut framed = Vec::with_capacity(4 + body_len);
    framed.extend_from_slice(&header);
    framed.extend_from_slice(&body);
    let (frame, rest) = FrameKind::decode(&framed).expect("decode frame");
    assert!(rest.is_empty(), "decoder did not consume entire frame");
    frame
}

/// Drive an async test body inside a `LocalSet` so the helpers can
/// call `spawn_local`. Wrapping at the function level (instead of
/// inside each test) keeps the test bodies focused on assertions.
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

#[test]
fn lifecycle_ping_pong() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone());
        let mut stream = wait_for_socket(&socket_path, Duration::from_secs(2)).await;

        let nonce = 0xCAFE_BABE_1234_5678_u64;
        let ping = encode_ping(nonce);
        stream.write_all(&ping).await.unwrap();
        stream.flush().await.unwrap();

        let frame = read_one_frame(&mut stream).await;
        assert_eq!(
            frame,
            FrameKind::Pong { nonce },
            "PONG nonce must match PING nonce",
        );

        // Trigger shutdown and let the server drain.
        drop(stream);
        shutdown_tx.send(()).ok();
        let result = server_handle.await.unwrap();
        assert!(result.is_ok(), "server returned: {result:?}");

        // Clean shutdown should remove the socket file.
        assert!(
            !socket_path.exists(),
            "socket {} should have been unlinked on shutdown",
            socket_path.display(),
        );
    });
}

#[test]
fn lifecycle_stale_socket() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // Simulate a dead-server crash: a leftover regular file at the path.
        std::fs::write(&socket_path, b"stale leftover").unwrap();
        assert!(socket_path.exists());

        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone());
        // The server should have bound successfully — verify with a quick PING.
        let mut stream = wait_for_socket(&socket_path, Duration::from_secs(2)).await;
        let ping = encode_ping(7);
        stream.write_all(&ping).await.unwrap();
        let frame = read_one_frame(&mut stream).await;
        assert_eq!(frame, FrameKind::Pong { nonce: 7 });

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}

#[test]
fn lifecycle_busy_socket() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        // Start server A.
        let (shutdown_a, handle_a) = spawn_server(socket_path.clone());
        let _probe = wait_for_socket(&socket_path, Duration::from_secs(2)).await;

        // Start server B at the same path; it should error with SocketBusy.
        let cfg_b = ServerConfig {
            socket_path: socket_path.clone(),
            pre_seeded_session: None,
            seed_with_pty: false,
            seed_command: None,
            ..ServerConfig::with_default_socket()
        };
        let server_b = ServerRuntime::new(cfg_b);
        let (_never_tx, never_rx) = oneshot::channel::<()>();
        let result_b = server_b
            .run_async(async move {
                let _ = never_rx.await;
            })
            .await;
        match result_b {
            Err(ServerError::SocketBusy(p)) => {
                assert_eq!(p, socket_path);
            }
            other => panic!("expected SocketBusy, got {other:?}"),
        }

        // Tear A down cleanly.
        shutdown_a.send(()).ok();
        handle_a.await.unwrap().unwrap();
    });
}

#[test]
fn lifecycle_partial_frame_disconnect() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");

        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone());
        let initial = wait_for_socket(&socket_path, Duration::from_secs(2)).await;

        // Connect, send only 2 of 4 length-prefix bytes, then drop.
        {
            let mut stream = initial;
            stream.write_all(&[0x00, 0x09]).await.unwrap();
            stream.flush().await.unwrap();
            // Drop (Tokio shuts down the write half on drop).
        }

        // Give the server a moment to process the disconnect.
        sleep(Duration::from_millis(50)).await;

        // The server must still be alive and accept a new connection.
        let mut stream2 = UnixStream::connect(&socket_path).await.unwrap();
        let nonce = 42_u64;
        stream2.write_all(&encode_ping(nonce)).await.unwrap();
        let frame = read_one_frame(&mut stream2).await;
        assert_eq!(frame, FrameKind::Pong { nonce });

        drop(stream2);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}
