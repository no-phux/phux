//! The attach handshake over WebSocket (the phux-web path): one frame per
//! binary message, HELLO -> ATTACH -> ATTACHED + bootstrap, a PING/PONG round
//! trip, and the fatal close for a zero `attach_id`.

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::wire::frame::{AttachTarget, ErrorCode, FrameKind};
use phux_server::{ServerConfig, ServerRuntime};
use phux_server_testkit::{
    WIRE_RECV_TIMEOUT, assert_protocol_error_detach, encode_frame_vec, run_local,
};
use tempfile::TempDir;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use super::common::attach;

type Ws = WebSocketStream<TcpStream>;

fn hello() -> FrameKind {
    FrameKind::Hello {
        client_name: "ws-attach-test".to_owned(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: ClientCapabilities::default(),
    }
}

async fn send(ws: &mut Ws, frame: &FrameKind) {
    ws.send(Message::Binary(encode_frame_vec(frame).into()))
        .await
        .unwrap();
}

/// The next decoded frame (or `None` once the socket closes).
async fn next(ws: &mut Ws) -> Option<FrameKind> {
    loop {
        match tokio::time::timeout(WIRE_RECV_TIMEOUT, ws.next())
            .await
            .expect("ws frame")
        {
            Some(Ok(Message::Binary(data))) => {
                let (frame, rest) = FrameKind::decode(&data).expect("decode server frame");
                assert!(rest.is_empty(), "one frame per binary message");
                return Some(frame);
            }
            Some(Ok(Message::Close(_))) | None => return None,
            Some(_) => {}
        }
    }
}

async fn connect(addr: SocketAddr) -> Ws {
    let url = format!("ws://{addr}/");
    for _ in 0..40 {
        if let Ok(tcp) = TcpStream::connect(&addr).await
            && let Ok((ws, _)) = tokio_tungstenite::client_async(&url, tcp).await
        {
            return ws;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("websocket never became connectable at {addr}");
}

#[test]
fn ws_hello_attach_receives_attached_and_snapshot() {
    // Hold the port until the server binds so no neighbour can take it.
    let hold = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = hold.local_addr().unwrap();
    run_local(async move {
        let tmp = TempDir::new().unwrap();
        let cfg = ServerConfig {
            socket_path: tmp.path().join("phux.sock"),
            pre_seeded_session: Some("default".to_owned()),
            seed_with_pty: false,
            seed_command: None,
            ..ServerConfig::with_default_socket()
        };
        let (_shutdown, stop) = oneshot::channel::<()>();
        drop(hold);
        let _server = tokio::task::spawn_local(async move {
            ServerRuntime::new(cfg)
                .listen_ws(addr)
                .run_async(async move {
                    let _ = stop.await;
                })
                .await
        });

        let mut ws = connect(addr).await;
        send(&mut ws, &hello()).await;
        send(
            &mut ws,
            &attach(AttachTarget::ByName("default".to_owned()), 80, 24),
        )
        .await;
        let (mut got_attached, mut got_snapshot) = (false, false);
        while !(got_attached && got_snapshot) {
            match next(&mut ws).await.expect("socket open") {
                FrameKind::Attached { .. } => got_attached = true,
                FrameKind::BootstrapBegin { cols, rows, .. } => {
                    assert!(cols > 0 && rows > 0);
                    got_snapshot = true;
                }
                _ => {}
            }
        }
        let nonce = 0xCAFE_BABE_1234_5678_u64;
        send(&mut ws, &FrameKind::Ping { nonce }).await;
        while !matches!(next(&mut ws).await, Some(FrameKind::Pong { nonce: n }) if n == nonce) {}
        drop(ws);

        // A zero correlation id is rejected before any attach state exists.
        let mut bad = connect(addr).await;
        send(&mut bad, &hello()).await;
        assert!(matches!(
            next(&mut bad).await,
            Some(FrameKind::HelloOk { .. })
        ));
        let zero = FrameKind::Attach {
            attach_id: 0,
            target: AttachTarget::ByName("default".to_owned()),
            viewport: phux_protocol::wire::frame::ViewportInfo::new(80, 24),
            request_scrollback: false,
            scrollback_limit_lines: 0,
            role_policy: None,
        };
        send(&mut bad, &zero).await;
        assert!(matches!(
            next(&mut bad).await,
            Some(FrameKind::Error { code: ErrorCode::MalformedMessage, message, .. })
                if message.contains("attach_id must be nonzero")
        ));
        assert_protocol_error_detach(&next(&mut bad).await.expect("DETACHED before close"));
        assert!(
            next(&mut bad).await.is_none(),
            "server closes the websocket"
        );
    });
}
