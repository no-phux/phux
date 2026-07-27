//! End-to-end over the WebTransport transport (phux-0wmf): a real native
//! WebTransport client performs the attach handshake (HELLO -> ATTACH) and
//! receives ATTACHED + TERMINAL_SNAPSHOT — exactly what the wasm browser
//! client (phux-web) does over its WebTransport path. This exercises the full
//! server-side wire path over HTTP/3-over-QUIC: session establishment, the
//! single bidirectional stream, and length-prefixed frame reassembly.

#![cfg(feature = "webtransport")]
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
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};
use phux_server::{ServerConfig, ServerError, ServerRuntime};
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, LocalSet};
use wtransport::ClientConfig;

/// A free UDP port for the WebTransport listener. Bind-and-drop is racy in
/// principle, but the window is a few milliseconds on loopback.
fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn spawn_wt_server(
    socket_path: std::path::PathBuf,
    seeded: &str,
    wt_addr: std::net::SocketAddr,
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
            .listen_webtransport(wt_addr)
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

/// Pull the next complete length-prefixed frame off `buf`, mirroring the
/// reassembly the wasm client's `framing::FrameBuffer` performs (a stream
/// chunk boundary is not a frame boundary).
fn next_frame(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    if buf.len() < 4 {
        return None;
    }
    let body_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let total = 4 + body_len;
    if buf.len() < total {
        return None;
    }
    let rest = buf.split_off(total);
    Some(std::mem::replace(buf, rest))
}

#[test]
fn wt_hello_attach_receives_attached_and_snapshot() {
    let port = free_udp_port();
    let wt_addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    // Pre-provision the cert pair in a tempdir and point the listener at it,
    // so the test never touches the user's real state dir.
    let tls_dir = TempDir::new().unwrap();
    let cert = tls_dir.path().join("cert.pem");
    let key = tls_dir.path().join("key.pem");
    phux_server::transport::tls::ensure_self_signed(&cert, &key).unwrap();
    unsafe {
        std::env::set_var("PHUX_WS_TLS_CERT", &cert);
        std::env::set_var("PHUX_WS_TLS_KEY", &key);
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = LocalSet::new();
    local.block_on(&rt, async move {
        let tmp = TempDir::new().unwrap();
        let (shutdown, server) = spawn_wt_server(tmp.path().join("phux.sock"), "default", wt_addr);

        // Connect over WebTransport, retrying until the listener is up. The
        // test client skips cert validation (the self-signed leaf is
        // throwaway); the HTTP/3 CONNECT handshake is still the real thing.
        let url = format!("https://127.0.0.1:{port}/session");
        let mut connection = None;
        for _ in 0..40 {
            let config = ClientConfig::builder()
                .with_bind_default()
                .with_no_cert_validation()
                .build();
            let endpoint = wtransport::Endpoint::client(config).unwrap();
            if let Ok(conn) = endpoint.connect(&url).await {
                connection = Some(conn);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let connection = connection.expect("webtransport connect");

        // One bidirectional stream carries the whole wire (the phux-web
        // contract): HELLO then ATTACH as length-prefixed frames.
        let (mut send, mut recv) = connection.open_bi().await.unwrap().await.unwrap();
        let hello = FrameKind::Hello {
            client_name: "wt-attach-test".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::default(),
        };
        send.write_all(&encode(&hello)).await.unwrap();
        let attach = FrameKind::Attach {
            target: AttachTarget::ByName("default".to_owned()),
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: false,
            scrollback_limit_lines: 0,
        };
        send.write_all(&encode(&attach)).await.unwrap();

        // Collect frames until both ATTACHED and a TERMINAL_SNAPSHOT arrive,
        // reassembling across arbitrary stream-chunk boundaries.
        let mut got_attached = false;
        let mut got_snapshot = false;
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        let deadline = tokio::time::sleep(HANDSHAKE_DEADLINE);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                () = &mut deadline => break,
                read = recv.read(&mut chunk) => {
                    let Ok(Some(n)) = read else { break };
                    buf.extend_from_slice(&chunk[..n]);
                    while let Some(framed) = next_frame(&mut buf) {
                        let (frame, _) = FrameKind::decode(&framed).expect("decode server frame");
                        match frame {
                            FrameKind::Attached { .. } => got_attached = true,
                            FrameKind::TerminalSnapshot { cols, rows, .. } => {
                                assert!(cols > 0 && rows > 0, "snapshot has a real grid");
                                got_snapshot = true;
                            }
                            _ => {}
                        }
                    }
                    if got_attached && got_snapshot { break; }
                }
            }
        }

        assert!(got_attached, "server sent ATTACHED");
        assert!(got_snapshot, "server sent TERMINAL_SNAPSHOT");

        drop((send, recv, connection));
        shutdown.send(()).ok();
        server.await.unwrap().unwrap();
    });
}
