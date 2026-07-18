//! Integration test for the WebSocket transport (phux-486.4).
//!
//! When `PHUX_WS_ADDR` is set, the server accepts WebSocket clients speaking the
//! *identical* length-prefixed `FrameKind` wire — one binary message per frame.
//! This mirrors the UDS `lifecycle_ping_pong` round-trip over WebSocket, proving
//! the new transport carries the real protocol end-to-end (RFC 6455 handshake +
//! frame round-trip), not just that it compiles.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(unused_unsafe, reason = "env::set_var is unsafe only on edition 2024")]

use std::time::Duration;

use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use phux_protocol::wire::frame::FrameKind;
use phux_server::{ServerConfig, ServerError, ServerRuntime};
use tempfile::TempDir;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, LocalSet};
use tokio_tungstenite::tungstenite::Message;

fn encode_ping(nonce: u64) -> BytesMut {
    let mut buf = BytesMut::new();
    FrameKind::Ping { nonce }.encode(&mut buf);
    buf
}

/// Grab an ephemeral port by binding `:0` and reading it back. Small race
/// window between drop and the server's bind, acceptable for a test.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn spawn_ws_server(
    socket_path: std::path::PathBuf,
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
        ServerRuntime::new(cfg)
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    (tx, handle)
}

#[test]
fn ws_lifecycle_ping_pong() {
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    // nextest runs each test in its own process, so this env var isn't shared.
    unsafe {
        std::env::set_var("PHUX_WS_ADDR", &addr);
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = LocalSet::new();
    local.block_on(&rt, async move {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_ws_server(socket_path);

        // Connect over WebSocket, retrying until the listener is up. We open the
        // TCP stream ourselves and drive `client_async` so no TLS/connect
        // feature is needed.
        let url = format!("ws://{addr}/");
        let mut ws = None;
        for _ in 0..40 {
            if let Ok(tcp) = TcpStream::connect(&addr).await
                && let Ok((stream, _resp)) = tokio_tungstenite::client_async(&url, tcp).await
            {
                ws = Some(stream);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let mut ws = ws.expect("websocket connect");

        let nonce = 0xCAFE_BABE_1234_5678_u64;
        ws.send(Message::Binary(encode_ping(nonce).to_vec()))
            .await
            .unwrap();

        // Expect a typed PONG binary message.
        let pong = loop {
            if let Message::Binary(data) = ws.next().await.expect("ws closed before PONG").unwrap()
            {
                break data;
            }
        };
        let (frame, rest) = FrameKind::decode(&pong).expect("decode PONG frame");
        assert!(rest.is_empty(), "decoder left trailing bytes");
        assert_eq!(
            frame,
            FrameKind::Pong { nonce },
            "PONG nonce must match PING nonce",
        );

        drop(ws);
        shutdown_tx.send(()).ok();
        let result = server_handle.await.unwrap();
        assert!(result.is_ok(), "server returned: {result:?}");
    });
}
