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
use phux_protocol::caps::{
    BootstrapCapabilities, ServerCapabilities, ServerFeature, ServerFeatureSet,
    select_bootstrap_profile,
};
use phux_protocol::ids::ResourceId;
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
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_bidi_streams(quinn::VarInt::from_u32(128));
    server_config.transport_config(Arc::new(transport));
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

#[allow(
    clippy::panic,
    reason = "the fixture must reject a non-HELLO first frame"
)]
async fn accept_hello(send: &mut quinn::SendStream, recv: &mut quinn::RecvStream) {
    accept_hello_with_caps(send, recv, ServerCapabilities::new()).await;
}

#[allow(
    clippy::panic,
    reason = "the fixture must reject a non-HELLO first frame"
)]
async fn accept_hello_with_caps(
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    server_caps: ServerCapabilities,
) {
    let FrameKind::Hello { client_caps, .. } = read_frame(recv).await else {
        panic!("expected HELLO");
    };
    if server_caps.features.contains(ServerFeature::QuicStreams) {
        assert!(
            client_caps.quic_streams,
            "QUIC_STREAMS selection requires an explicit client offer"
        );
    }
    let (selected_profile, bootstrap_limits) =
        select_bootstrap_profile(&client_caps, &BootstrapCapabilities::new())
            .expect("fixture profiles intersect");
    write_frame(
        send,
        &FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps,
            server_id: Vec::new(),
            selected_profile,
            bootstrap_limits,
        },
    )
    .await;
}

async fn read_stream_bind(
    recv: &mut quinn::RecvStream,
) -> phux_protocol::wire::stream_bind::StreamBind {
    let mut prefix = [0_u8; 4];
    recv.read_exact(&mut prefix).await.unwrap();
    let len = u32::from_be_bytes(prefix) as usize;
    let mut bytes = Vec::with_capacity(4 + len);
    bytes.extend_from_slice(&prefix);
    bytes.resize(4 + len, 0);
    recv.read_exact(&mut bytes[4..]).await.unwrap();
    let (bind, used) = phux_protocol::wire::stream_bind::decode(&bytes).unwrap();
    assert_eq!(used, bytes.len());
    bind
}

const fn ack(seq: u64) -> FrameKind {
    FrameKind::FrameAck {
        terminal_id: ResourceId::Local { id: 1 },
        stream_id: phux_protocol::StreamId::new(1).expect("stream"),
        bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
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
async fn negotiated_quic_streams_bind_route_and_merge_terminal_frames() {
    let (_dir, cert, key) = cert_pair();
    let (endpoint, addr) = server_endpoint(&cert, &key);
    let terminal_id = ResourceId::local(9);
    let from_client = FrameKind::FrameAck {
        terminal_id: terminal_id.clone(),
        stream_id: phux_protocol::StreamId::new(1).unwrap(),
        bootstrap_id: phux_protocol::BootstrapId::new(1).unwrap(),
        seq: 11,
    };
    let from_server = ack(22);

    let server = {
        let terminal_id = terminal_id.clone();
        let from_client = from_client.clone();
        let from_server = from_server.clone();
        async move {
            let conn = endpoint.accept().await.unwrap().await.unwrap();
            let (mut control_send, mut control_recv) = conn.accept_bi().await.unwrap();
            accept_hello_with_caps(
                &mut control_send,
                &mut control_recv,
                ServerCapabilities::new()
                    .with_features(ServerFeatureSet::with(&[ServerFeature::QuicStreams])),
            )
            .await;
            let (mut terminal_send, mut terminal_recv) = conn.accept_bi().await.unwrap();
            let bind = read_stream_bind(&mut terminal_recv).await;
            assert_eq!(bind.terminal_id, terminal_id);
            assert_eq!(bind.stream_id.get(), 1);
            write_frame(&mut terminal_send, &from_server).await;
            assert_eq!(read_frame(&mut terminal_recv).await, from_client);
            terminal_send.finish().unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            write_frame(&mut control_send, &FrameKind::Pong { nonce: 99 }).await;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    };

    let client = async move {
        let dial = QuicDial {
            addr,
            server_name: "localhost".to_owned(),
            token: None,
            trust: CertTrust::SkipVerify,
        };
        let mut conn = Connection::connect_quic(&dial).await.expect("dial");
        assert!(conn.multistream_enabled());
        conn.bind_terminal(&terminal_id).await.expect("bind");
        conn.send(&from_client)
            .await
            .expect("send on Terminal stream");
        let received = conn.recv().await.expect("receive Terminal stream frame");
        assert_eq!(
            conn.recv()
                .await
                .expect("control progresses after stream end"),
            FrameKind::Pong { nonce: 99 }
        );
        let error = conn
            .send(&from_client)
            .await
            .expect_err("Terminal traffic cannot fall back to control after stream end");
        assert!(error.to_string().contains("requires a live QUIC binding"));
        received
    };

    let (_server, got) = tokio::join!(server, client);
    assert_eq!(got, from_server);
}

#[tokio::test]
async fn malformed_terminal_stream_length_fails_promptly() {
    let (_dir, cert, key) = cert_pair();
    let (endpoint, addr) = server_endpoint(&cert, &key);
    let terminal_id = ResourceId::local(9);

    let server_terminal_id = terminal_id.clone();
    let server = async move {
        let conn = endpoint.accept().await.unwrap().await.unwrap();
        let (mut control_send, mut control_recv) = conn.accept_bi().await.unwrap();
        accept_hello_with_caps(
            &mut control_send,
            &mut control_recv,
            ServerCapabilities::new()
                .with_features(ServerFeatureSet::with(&[ServerFeature::QuicStreams])),
        )
        .await;
        let (mut terminal_send, mut terminal_recv) = conn.accept_bi().await.unwrap();
        let bind = read_stream_bind(&mut terminal_recv).await;
        assert_eq!(bind.terminal_id, server_terminal_id);
        terminal_send.write_all(&[0, 0, 0, 0]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };

    let client = async move {
        let dial = QuicDial {
            addr,
            server_name: "localhost".to_owned(),
            token: None,
            trust: CertTrust::SkipVerify,
        };
        let mut conn = Connection::connect_quic(&dial).await.expect("dial");
        conn.bind_terminal(&terminal_id).await.expect("bind");
        tokio::time::timeout(std::time::Duration::from_secs(2), conn.recv())
            .await
            .expect("malformed length fails promptly")
            .expect_err("zero-length frame is invalid")
            .to_string()
    };

    let ((), error) = tokio::join!(server, client);
    assert!(
        error.contains("frame length 0"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn client_terminal_stream_cap_accounts_for_the_control_stream() {
    let (_dir, cert, key) = cert_pair();
    let (endpoint, addr) = server_endpoint(&cert, &key);

    let server = async move {
        let conn = endpoint.accept().await.unwrap().await.unwrap();
        let (mut control_send, mut control_recv) = conn.accept_bi().await.unwrap();
        accept_hello_with_caps(
            &mut control_send,
            &mut control_recv,
            ServerCapabilities::new()
                .with_features(ServerFeatureSet::with(&[ServerFeature::QuicStreams])),
        )
        .await;
        let mut streams = Vec::new();
        for expected in 1..=127 {
            let (send, mut recv) = conn.accept_bi().await.unwrap();
            let bind = read_stream_bind(&mut recv).await;
            assert_eq!(bind.terminal_id, ResourceId::local(expected));
            streams.push((send, recv));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        streams
    };

    let client = async move {
        let dial = QuicDial {
            addr,
            server_name: "localhost".to_owned(),
            token: None,
            trust: CertTrust::SkipVerify,
        };
        let mut conn = Connection::connect_quic(&dial).await.expect("dial");
        for id in 1..=127 {
            conn.bind_terminal(&ResourceId::local(id))
                .await
                .expect("127 Terminal streams fit beside control");
        }
        let error = conn
            .bind_terminal(&ResourceId::local(128))
            .await
            .expect_err("the 128th Terminal stream exceeds the connection cap")
            .to_string();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        error
    };

    let (_streams, error) = tokio::join!(server, client);
    assert!(
        error.contains("cap exceeded (127)"),
        "unexpected error: {error}"
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
