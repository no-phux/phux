//! The attach handshake over WebTransport (HTTP/3 over QUIC; the phux-web
//! path): one bidirectional stream carries length-prefixed frames that must
//! be reassembled across arbitrary chunk boundaries.

#![cfg(feature = "webtransport")]

use std::time::Duration;

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::wire::frame::{AttachTarget, FrameKind};
use phux_server::{ServerConfig, ServerRuntime};
use phux_server_testkit::{WIRE_RECV_TIMEOUT, encode_frame_vec, run_local};
use tempfile::TempDir;
use tokio::sync::oneshot;
use wtransport::ClientConfig;

use super::common::attach;

#[test]
fn wt_hello_attach_receives_attached_and_snapshot() {
    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let wt_addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    // A throwaway cert pair, so the test never touches the real state dir.
    let tls_dir = TempDir::new().unwrap();
    let (cert, key) = (
        tls_dir.path().join("cert.pem"),
        tls_dir.path().join("key.pem"),
    );
    phux_server::transport::tls::ensure_self_signed(&cert, &key).unwrap();

    run_local(async move {
        let tmp = TempDir::new().unwrap();
        let cfg = ServerConfig {
            socket_path: tmp.path().join("phux.sock"),
            pre_seeded_session: Some("default".to_owned()),
            seed_with_pty: false,
            seed_command: None,
            env: phux_server::ServerEnv {
                tls_cert: Some(cert),
                tls_key: Some(key),
                ..phux_server::ServerEnv::default()
            },
            ..ServerConfig::with_default_socket()
        };
        let (_shutdown, stop) = oneshot::channel::<()>();
        let _server = tokio::task::spawn_local(async move {
            ServerRuntime::new(cfg)
                .listen_webtransport(wt_addr)
                .run_async(async move {
                    let _ = stop.await;
                })
                .await
        });

        // The client skips cert validation; the CONNECT handshake is real.
        let url = format!("https://127.0.0.1:{port}/session");
        let mut connection = None;
        for _ in 0..40 {
            // IPv4 loopback like the server, not the dual-stack default:
            // macOS can hand a dual-stack socket an ephemeral port another
            // IPv4 socket already owns, and that socket then eats replies.
            let config = ClientConfig::builder()
                .with_bind_address(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
                .with_no_cert_validation()
                .build();
            if let Ok(conn) = wtransport::Endpoint::client(config)
                .unwrap()
                .connect(&url)
                .await
            {
                connection = Some(conn);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let connection = connection.expect("webtransport connect");
        let (mut send, mut recv) = connection.open_bi().await.unwrap().await.unwrap();
        let hello = FrameKind::Hello {
            client_name: "wt-attach-test".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::default(),
        };
        send.write_all(&encode_frame_vec(&hello)).await.unwrap();
        let attach = attach(AttachTarget::ByName("default".to_owned()), 80, 24);
        send.write_all(&encode_frame_vec(&attach)).await.unwrap();

        let (mut got_attached, mut got_snapshot) = (false, false);
        let mut buf = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        tokio::time::timeout(WIRE_RECV_TIMEOUT, async {
            while !(got_attached && got_snapshot) {
                let n = recv.read(&mut chunk).await.unwrap().expect("stream open");
                buf.extend_from_slice(&chunk[..n]);
                while buf.len() >= 4 {
                    let total = 4 + u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
                    if buf.len() < total {
                        break;
                    }
                    let (frame, _) = FrameKind::decode(&buf[..total]).expect("decode server frame");
                    buf.drain(..total);
                    match frame {
                        FrameKind::Attached { .. } => got_attached = true,
                        FrameKind::BootstrapBegin { cols, rows, .. } => {
                            assert!(cols > 0 && rows > 0);
                            got_snapshot = true;
                        }
                        _ => {}
                    }
                }
            }
        })
        .await
        .expect("ATTACHED and a bootstrap over WebTransport");
    });
}
