//! End-to-end over the WebSocket transport (phux-486.4/.5): a real client
//! performs the attach handshake (HELLO -> ATTACH) and receives ATTACHED +
//! TERMINAL_SNAPSHOT — exactly what the wasm browser client (phux-web) does.
//! This exercises the full server-side wire path over WebSocket.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::doc_markdown, reason = "tests")]
#![allow(unused_unsafe, reason = "env::set_var is unsafe only on edition 2024")]

use std::time::Duration;

/// Ceiling for the attach handshake drain below.
///
/// Not load-bearing: the assertions are that ATTACHED and TERMINAL_SNAPSHOT
/// both arrive and that the snapshot carries a real grid — never how fast.
/// This file has no `mod common;`, so it cannot borrow the shared
/// `WIRE_RECV_TIMEOUT`; the value matches it deliberately. The 5s it replaces
/// was generous on an idle laptop and a measurement of the scheduler on a
/// saturated one (phux-br1f). A server that never attaches still fails.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(15);

use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::wire::frame::{AttachTarget, ErrorCode, FrameKind, ViewportInfo};
use phux_server::{ServerConfig, ServerError, ServerRuntime};
use tempfile::TempDir;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, LocalSet};
use tokio_tungstenite::tungstenite::Message;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn spawn_ws_server(
    socket_path: std::path::PathBuf,
    seeded: &str,
) -> (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>) {
    let (tx, rx) = oneshot::channel::<()>();
    let cfg = ServerConfig {
        socket_path,
        pre_seeded_session: Some(seeded.to_owned()),
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

fn encode(frame: &FrameKind) -> Vec<u8> {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf.to_vec()
}

#[test]
fn ws_hello_attach_receives_attached_and_snapshot() {
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
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
        let (shutdown, server) = spawn_ws_server(tmp.path().join("phux.sock"), "default");

        // Connect over WebSocket, retrying until the listener is up.
        let url = format!("ws://{addr}/");
        let mut ws = None;
        for _ in 0..40 {
            if let Ok(tcp) = TcpStream::connect(&addr).await
                && let Ok((s, _)) = tokio_tungstenite::client_async(&url, tcp).await
            {
                ws = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let mut ws = ws.expect("websocket connect");

        // HELLO then ATTACH to the seeded "default" session — one frame per
        // binary message, the phux-web contract.
        let hello = FrameKind::Hello {
            client_name: "ws-attach-test".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::default(),
        };
        ws.send(Message::Binary(encode(&hello))).await.unwrap();
        let attach = FrameKind::Attach {
            attach_id: 1,
            target: AttachTarget::ByName("default".to_owned()),
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: false,
            scrollback_limit_lines: 0,
        };
        ws.send(Message::Binary(encode(&attach))).await.unwrap();

        // Collect frames until both ATTACHED and a TERMINAL_SNAPSHOT arrive.
        let mut got_attached = false;
        let mut got_snapshot = false;
        let deadline = tokio::time::sleep(HANDSHAKE_DEADLINE);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                () = &mut deadline => break,
                msg = ws.next() => {
                    let Some(Ok(Message::Binary(data))) = msg else { continue };
                    let (frame, _) = FrameKind::decode(&data).expect("decode server frame");
                    match frame {
                        FrameKind::Attached { .. } => got_attached = true,
                        FrameKind::BootstrapBegin { cols, rows, .. } => {
                            assert!(cols > 0 && rows > 0, "snapshot has a real grid");
                            got_snapshot = true;
                        }
                        _ => {}
                    }
                    if got_attached && got_snapshot { break; }
                }
            }
        }

        assert!(got_attached, "server sent ATTACHED");
        assert!(got_snapshot, "server sent TERMINAL_SNAPSHOT");

        drop(ws);

        // A fresh web connection must reject the reserved zero correlation id
        // before creating any attached consumer state.
        let tcp = TcpStream::connect(&addr).await.unwrap();
        let (mut bad_ws, _) = tokio_tungstenite::client_async(&url, tcp).await.unwrap();
        bad_ws
            .send(Message::Binary(encode(&FrameKind::Hello {
                client_name: "ws-zero-attach-test".to_owned(),
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                client_caps: ClientCapabilities::default(),
            })))
            .await
            .unwrap();
        let Some(Ok(Message::Binary(hello_ok))) = bad_ws.next().await else {
            panic!("server must answer HELLO");
        };
        assert!(matches!(
            FrameKind::decode(&hello_ok).unwrap().0,
            FrameKind::HelloOk { .. }
        ));
        bad_ws
            .send(Message::Binary(encode(&FrameKind::Attach {
                attach_id: 0,
                target: AttachTarget::ByName("default".to_owned()),
                viewport: ViewportInfo::new(80, 24),
                request_scrollback: false,
                scrollback_limit_lines: 0,
            })))
            .await
            .unwrap();
        let Some(Ok(Message::Binary(error))) = bad_ws.next().await else {
            panic!("server must flush zero-id error");
        };
        assert!(matches!(
            FrameKind::decode(&error).unwrap().0,
            FrameKind::Error {
                code: ErrorCode::MalformedMessage,
                message,
                ..
            } if message.contains("attach_id must be nonzero")
        ));
        let closed = tokio::time::timeout(HANDSHAKE_DEADLINE, bad_ws.next())
            .await
            .expect("server closes websocket");
        assert!(matches!(closed, None | Some(Ok(Message::Close(_)))));
        shutdown.send(()).ok();
        server.await.unwrap().unwrap();
    });
}
