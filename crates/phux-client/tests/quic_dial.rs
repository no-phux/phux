//! QUIC dialer round-trip (`phux-y8v6`, ADR-0007).
//!
//! Stands up a minimal quinn server with the same TLS shape the real
//! `QuicListener` uses — TLS 1.3, the `phux-quic/1` ALPN, a self-signed cert —
//! and drives [`Connection::connect_quic`] against it. This exercises the
//! client side end-to-end: the TLS handshake (ALPN negotiation + signature
//! verification), the optional bearer-token preamble, and the SPEC §5 frame
//! framing in both directions. The server's own acceptance of these frames is
//! covered by `phux-server`'s `transport::quic` tests; this is the mirror.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use bytes::BytesMut;
use phux_client::attach::connection::Connection;
use phux_client::attach::{CertTrust, QuicDial};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::ServerCapabilities;
use phux_protocol::ids::TerminalId;
use phux_protocol::policy::QUIC_ALPN;
use phux_protocol::wire::frame::FrameKind;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// A self-signed cert + key in a fresh tempdir, kept alive for the test.
fn cert_pair() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    phux_server::transport::tls::ensure_self_signed(&cert, &key).unwrap();
    (dir, cert, key)
}

/// A quinn server endpoint on an OS-assigned loopback port, TLS 1.3 + the phux
/// ALPN, terminating with the given self-signed cert — the same shape the real
/// `QuicListener` builds.
fn server_endpoint(cert: &Path, key: &Path) -> (quinn::Endpoint, SocketAddr) {
    let certs = CertificateDer::pem_file_iter(cert)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_file(key).unwrap();
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .unwrap();
    tls.alpn_protocols = vec![QUIC_ALPN.to_vec()];
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap();
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    let endpoint = quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = endpoint.local_addr().unwrap();
    (endpoint, addr)
}

/// Read one length-prefixed phux frame off a quinn recv stream.
async fn read_frame(recv: &mut quinn::RecvStream) -> FrameKind {
    let mut header = [0u8; 4];
    recv.read_exact(&mut header).await.unwrap();
    let len = u32::from_be_bytes(header) as usize;
    let mut framed = header.to_vec();
    framed.resize(4 + len, 0);
    recv.read_exact(&mut framed[4..]).await.unwrap();
    FrameKind::decode(&framed).unwrap().0
}

/// Write one length-prefixed phux frame onto a quinn send stream.
async fn write_frame(send: &mut quinn::SendStream, frame: &FrameKind) {
    let mut out = BytesMut::new();
    frame.encode(&mut out);
    send.write_all(&out).await.unwrap();
}

async fn accept_hello(send: &mut quinn::SendStream, recv: &mut quinn::RecvStream) {
    assert!(matches!(read_frame(recv).await, FrameKind::Hello { .. }));
    write_frame(
        send,
        &FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new(),
            server_id: Vec::new(),
        },
    )
    .await;
}

const fn ack(seq: u64) -> FrameKind {
    FrameKind::FrameAck {
        terminal_id: TerminalId::Local { id: 1 },
        seq,
    }
}

#[tokio::test]
async fn loopback_skip_verify_round_trips_both_directions() {
    let (_dir, cert, key) = cert_pair();
    let (endpoint, addr) = server_endpoint(&cert, &key);

    let from_client = ack(11);
    let from_server = ack(22);

    let server = {
        let from_client = from_client.clone();
        let from_server = from_server.clone();
        async move {
            let conn = endpoint.accept().await.unwrap().await.unwrap();
            let (mut send, mut recv) = conn.accept_bi().await.unwrap();
            accept_hello(&mut send, &mut recv).await;
            // Client → server.
            assert_eq!(read_frame(&mut recv).await, from_client);
            // Server → client.
            write_frame(&mut send, &from_server).await;
            send.finish().unwrap();
            // Keep the connection alive until the client has read the reply.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    };

    let client = async {
        let dial = QuicDial {
            addr,
            server_name: "localhost".to_owned(),
            token: None,
            trust: CertTrust::SkipVerify,
        };
        let mut conn = Connection::connect_quic(&dial).await.expect("dial");
        conn.send(&from_client).await.expect("send");
        conn.recv().await.expect("recv")
    };

    let (_server, got) = tokio::join!(server, client);
    assert_eq!(
        got, from_server,
        "server's frame round-trips back to client"
    );
}

#[tokio::test]
async fn pinned_fingerprint_accepts_matching_cert() {
    let (_dir, cert, key) = cert_pair();
    let (endpoint, addr) = server_endpoint(&cert, &key);
    let fingerprint = phux_server::transport::tls::cert_fingerprint(&cert).unwrap();

    let frame = ack(7);

    let server = async move {
        let conn = endpoint.accept().await.unwrap().await.unwrap();
        let (mut send, mut recv) = conn.accept_bi().await.unwrap();
        accept_hello(&mut send, &mut recv).await;
        read_frame(&mut recv).await
    };

    let client = {
        let frame = frame.clone();
        async move {
            let dial = QuicDial {
                addr,
                server_name: "localhost".to_owned(),
                token: None,
                trust: CertTrust::Pinned(fingerprint),
            };
            let mut conn = Connection::connect_quic(&dial).await.expect("pinned dial");
            conn.send(&frame).await.expect("send");
            // Hold open until the server reads.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    };

    let (got, ()) = tokio::join!(server, client);
    assert_eq!(got, frame, "a matching pin completes the handshake");
}

#[tokio::test]
async fn wrong_fingerprint_is_rejected() {
    let (_dir, cert, key) = cert_pair();
    let (endpoint, addr) = server_endpoint(&cert, &key);

    // Drive the server accept on a detached task; it never completes because the
    // client aborts the handshake at certificate verification.
    let server = tokio::spawn(async move {
        if let Some(incoming) = endpoint.accept().await {
            let _ = incoming.await;
        }
    });

    let dial = QuicDial {
        addr,
        server_name: "localhost".to_owned(),
        token: None,
        // A 32-byte all-zero fingerprint cannot match the real leaf.
        trust: CertTrust::Pinned("00".repeat(32)),
    };
    let result = Connection::connect_quic(&dial).await;
    assert!(
        result.is_err(),
        "a mismatched certificate pin must refuse the connection"
    );
    server.abort();
}

#[tokio::test]
async fn shutdown_closes_connection_promptly() {
    // The reconnect probe (and any clean teardown) must close the QUIC
    // connection at once — a CONNECTION_CLOSE — rather than leaving the server
    // to reap a phantom connection at its 30s idle timeout.
    let (_dir, cert, key) = cert_pair();
    let (endpoint, addr) = server_endpoint(&cert, &key);

    let server = async move {
        let conn = endpoint.accept().await.unwrap().await.unwrap();
        let (mut send, mut recv) = conn.accept_bi().await.unwrap();
        accept_hello(&mut send, &mut recv).await;
        tokio::time::timeout(std::time::Duration::from_secs(5), conn.closed())
            .await
            .expect("server must observe a prompt close, not the 30s idle timeout")
    };

    let client = async {
        let dial = QuicDial {
            addr,
            server_name: "localhost".to_owned(),
            token: None,
            trust: CertTrust::SkipVerify,
        };
        let conn = Connection::connect_quic(&dial).await.expect("dial");
        conn.shutdown().await;
    };

    let (closed, ()) = tokio::join!(server, client);
    assert!(
        matches!(
            closed,
            quinn::ConnectionError::ApplicationClosed(_) | quinn::ConnectionError::LocallyClosed
        ),
        "expected an application/local close, got {closed:?}"
    );
}

#[tokio::test]
async fn token_preamble_precedes_frames() {
    let (_dir, cert, key) = cert_pair();
    let (endpoint, addr) = server_endpoint(&cert, &key);

    let token = vec![0xABu8; 32];
    let frame = ack(99);

    let server = {
        let token = token.clone();
        let frame = frame.clone();
        async move {
            let conn = endpoint.accept().await.unwrap().await.unwrap();
            let (mut send, mut recv) = conn.accept_bi().await.unwrap();
            // The dialer writes the auth preamble (len: u32 BE + token) ahead of
            // any phux frame.
            let mut len_buf = [0u8; 4];
            recv.read_exact(&mut len_buf).await.unwrap();
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut got_token = vec![0u8; len];
            recv.read_exact(&mut got_token).await.unwrap();
            assert_eq!(got_token, token, "raw token bytes arrive first");
            accept_hello(&mut send, &mut recv).await;
            // Then the frame.
            assert_eq!(read_frame(&mut recv).await, frame);
        }
    };

    let client = {
        let token = token.clone();
        let frame = frame.clone();
        async move {
            let dial = QuicDial {
                addr,
                server_name: "localhost".to_owned(),
                token: Some(token),
                trust: CertTrust::SkipVerify,
            };
            let mut conn = Connection::connect_quic(&dial).await.expect("dial");
            conn.send(&frame).await.expect("send");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    };

    tokio::join!(server, client);
}
