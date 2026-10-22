use super::*;

use super::super::tls::ensure_self_signed;
use wtransport::ClientConfig;
use wtransport::endpoint::ConnectOptions;
use wtransport_proto::qpack::Decoder;

const TEST_TOKEN: [u8; crate::auth::TOKEN_LEN] = [0x11; crate::auth::TOKEN_LEN];
/// One complete framed message: 4-byte length prefix (body = 3) + body.
const FRAME: [u8; 7] = [0, 0, 0, 3, 0xde, 0xad, 0xbe];
/// A second frame for the echo direction (server -> client).
const ECHO_FRAME: [u8; 6] = [0, 0, 0, 2, 0xca, 0xfe];
/// Hang guard for the no-stream admission wait, never a timing assertion.
const HANG_GUARD: Duration = Duration::from_secs(60);

/// A token store file holding the one known [`TEST_TOKEN`].
fn token_store() -> (
    tempfile::NamedTempFile,
    Arc<crate::auth::ReloadingTokenStore>,
) {
    let file = tempfile::NamedTempFile::new().unwrap();
    crate::auth::write_test_credential(file.path(), &TEST_TOKEN);
    let store = crate::auth::ReloadingTokenStore::load(file.path().to_path_buf()).unwrap();
    (file, Arc::new(store))
}

/// A native client that skips certificate validation; the TLS handshake
/// and HTTP/3 CONNECT are still exercised end to end.
///
/// It binds IPv4 loopback like the listener, never the dual-stack
/// default: macOS picks a dual-stack socket's ephemeral port without
/// checking IPv4 bindings, and when it collides with another process's
/// IPv4 socket that socket receives every reply, so the handshake
/// silently times out after 30 s (about 1 run in 300 on a busy host).
fn client_endpoint() -> wtransport::Endpoint<wtransport::endpoint::endpoint_side::Client> {
    let config = ClientConfig::builder()
        .with_bind_address(SocketAddr::from(([127, 0, 0, 1], 0)))
        .with_no_cert_validation()
        .build();
    wtransport::Endpoint::client(config).unwrap()
}

/// A loopback listener with a fresh self-signed pair, and its session
/// URL base (`https://127.0.0.1:<port>/session`).
fn listener(
    tokens: Option<Arc<crate::auth::ReloadingTokenStore>>,
) -> (tempfile::TempDir, WtListener, String) {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    ensure_self_signed(&cert, &key).unwrap();
    let listener =
        WtListener::from_pem("127.0.0.1:0".parse().unwrap(), &cert, &key, tokens, None).unwrap();
    let url = format!(
        "https://127.0.0.1:{}/session",
        listener.local_addr().unwrap().port()
    );
    (dir, listener, url)
}

/// The listener accepts nothing while `options` is refused.
async fn assert_refused(listener: &WtListener, options: ConnectOptions) {
    let client = async {
        assert!(client_endpoint().connect(options).await.is_err());
    };
    tokio::select! {
        () = client => {}
        _ = listener.accept() => panic!("a refused CONNECT was accepted"),
    }
}

/// A `paired` listener's end-to-end session (ADR-0154 item 6): an
/// enrolled certificate presented inside the stream is the connection's
/// credential; a plain stream (what a browser sends) is refused.
#[tokio::test]
async fn under_paired_only_an_inner_session_with_an_enrolled_certificate_is_admitted() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let paths = crate::workload::WorkloadPaths {
        ca_cert: dir.path().join("workload-ca.pem"),
        ca_key: dir.path().join("workload-ca.key"),
        registry: dir.path().join("workload-keys"),
    };
    let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
    super::super::tls::ensure_server_identity(&cert, &key, &[], &paths).unwrap();
    let client_key = rcgen::KeyPair::generate().unwrap();
    let csr = rcgen::CertificateParams::new(Vec::<String>::new())
        .unwrap()
        .serialize_request(&client_key)
        .unwrap();
    let material =
        crate::workload::ClientMaterial::from_pem(csr.pem().unwrap().as_bytes()).unwrap();
    let expires = chrono::Utc::now().timestamp() + 3600;
    let prepared = crate::workload::prepare_enrollment(&paths, &material, expires).unwrap();
    let enrolled = prepared
        .commit(&paths.registry, vec!["*@global".to_owned()], expires)
        .unwrap();
    let (client_cert, client_key_path) = (dir.path().join("c.pem"), dir.path().join("c.key"));
    std::fs::write(&client_cert, prepared.issued_chain_pem().unwrap()).unwrap();
    std::fs::write(&client_key_path, client_key.serialize_pem()).unwrap();
    std::fs::set_permissions(&client_key_path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let ca = crate::workload::authority_certificate(&paths.ca_cert).unwrap();
    let registry =
        Arc::new(crate::workload::ReloadingWorkloadRegistry::load(paths.registry.clone()).unwrap());
    let inner =
        super::super::inner_tls::InnerTls::new(&cert, &key, Some((&ca, registry)), paths.clone())
            .unwrap();
    let listener = WtListener::from_pem(
        "127.0.0.1:0".parse().unwrap(),
        &cert,
        &key,
        None,
        Some(Arc::new(inner)),
    )
    .unwrap();
    let url = format!(
        "https://127.0.0.1:{}/session",
        listener.local_addr().unwrap().port()
    );

    // A browser's plain stream: refused before any frame is read.
    let plain = async {
        let conn = client_endpoint().connect(url.clone()).await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap().await.unwrap();
        send.write_all(&FRAME).await.unwrap();
        let mut byte = [0_u8; 1];
        assert!(recv.read_exact(&mut byte).await.is_err(), "refused");
    };
    tokio::time::timeout(HANG_GUARD, async {
        tokio::select! {
            () = plain => {}
            _ = listener.accept() => panic!("a plain stream was admitted under paired"),
        }
    })
    .await
    .unwrap();

    let client = async {
        let conn = client_endpoint().connect(url).await.unwrap();
        let (send, recv) = conn.open_bi().await.unwrap().await.unwrap();
        let config = phux_dial::tls::client_config_with_identity(
            &phux_dial::CertTrust::Authority {
                ca: crate::workload::ca_fingerprint(&paths.ca_cert).unwrap(),
            },
            &phux_dial::TlsClientIdentity::PemFiles {
                certificate: client_cert,
                private_key: client_key_path,
            },
            Some(phux_protocol::policy::QUIC_ALPN),
        )
        .unwrap();
        let mut tls = tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(
                rustls::pki_types::ServerName::try_from("localhost").unwrap(),
                tokio::io::join(recv, send),
            )
            .await
            .unwrap();
        tls.write_all(&FRAME).await.unwrap();
        tls.flush().await.unwrap();
        // Hold the session until the server has read the frame.
        let mut byte = [0_u8; 1];
        let _ = tokio::time::timeout(Duration::from_secs(2), tls.read(&mut byte)).await;
        conn
    };
    let server = async {
        let (mut reader, _writer, peer) = listener.accept().await.unwrap();
        (reader.read_frame().await.unwrap(), peer)
    };
    let (_conn, (frame, peer)) = tokio::join!(client, server);
    assert_eq!(frame.unwrap().as_ref(), &FRAME);
    assert_eq!(peer.mcp_host_key.as_deref(), Some(enrolled.id.as_str()));
}

#[tokio::test]
async fn round_trips_frames_unauthenticated() {
    let (_dir, listener, url) = listener(None);
    let server = async {
        let (mut reader, mut writer, peer) = listener.accept().await.unwrap();
        let got = reader.read_frame().await.unwrap();
        writer.write_frame(&ECHO_FRAME).await.unwrap();
        // Hold the session open until the client has read the echo.
        let _ = reader.read_frame().await;
        (got, peer)
    };
    let client = async {
        let conn = client_endpoint().connect(url).await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap().await.unwrap();
        send.write_all(&FRAME).await.unwrap();
        let mut echoed = [0u8; ECHO_FRAME.len()];
        recv.read_exact(&mut echoed).await.unwrap();
        echoed
    };

    let ((got, peer), echoed) = tokio::join!(server, client);
    assert_eq!(got.unwrap().as_ref(), &FRAME);
    assert_eq!(echoed, ECHO_FRAME);
    assert_eq!(peer.transport, TransportType::WebTransport);
    assert!(peer.mcp_host_key.is_none());
}

/// With the acks cut off, the writer stops near one congestion window
/// plus the unsent slack instead of the client's full stream credit.
#[tokio::test]
async fn writer_blocks_near_the_congestion_window_when_the_path_stalls() {
    let (_dir, listener, _) = listener(None);
    let proxy = phux_dial::testing::DropProxy::start(listener.local_addr().unwrap())
        .await
        .unwrap();
    let url = format!("https://127.0.0.1:{}/session", proxy.addr().port());

    let client = async {
        let conn = client_endpoint().connect(url).await.unwrap();
        let (mut send, recv) = conn.open_bi().await.unwrap().await.unwrap();
        send.write_all(&FRAME).await.unwrap();
        (conn, send, recv)
    };
    let server = async {
        let (mut reader, writer, _peer) = listener.accept().await.unwrap();
        reader.read_frame().await.unwrap();
        (reader, writer)
    };
    let (_client, (_reader, mut writer)) = tokio::join!(client, server);

    proxy.blackhole_downstream();
    let WtSend::Plain(send) = &mut writer.send else {
        panic!("a plain session");
    };
    let chunk = vec![0x5a_u8; 64 * 1024];
    let mut accepted = 0_usize;
    while accepted < 8 * 1024 * 1024 {
        match tokio::time::timeout(Duration::from_millis(300), send.write(&chunk)).await {
            Ok(Ok(written)) => accepted += written,
            Ok(Err(err)) => panic!("write failed: {err}"),
            Err(_) => break,
        }
    }
    let cwnd = send.window().connection().stats().path.cwnd;
    let bound = phux_dial::window::send_window_for(cwnd).max(64 * 1024);
    assert!(
        accepted as u64 <= bound,
        "WtWriter accepted {accepted} bytes against a {cwnd}-byte congestion window"
    );
}

/// Both carriers authenticate: the native header and the browser's URL
/// query parameter.
#[tokio::test]
async fn bearer_header_and_query_token_authenticate() {
    let token = hex::encode(TEST_TOKEN);
    let (_tokens, store) = token_store();
    let (_dir, listener, url) = listener(Some(store));
    let carriers = [
        ConnectOptions::builder(&url)
            .add_header("authorization", format!("Bearer {token}"))
            .build(),
        ConnectOptions::builder(format!("{url}?token={token}")).build(),
    ];
    for options in carriers {
        let server = async {
            let (mut reader, _writer, peer) = listener.accept().await.unwrap();
            (reader.read_frame().await.unwrap(), peer)
        };
        let client = async {
            let conn = client_endpoint().connect(options).await.unwrap();
            let (mut send, _recv) = conn.open_bi().await.unwrap().await.unwrap();
            send.write_all(&FRAME).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let ((got, peer), ()) = tokio::join!(server, client);
        assert_eq!(got.unwrap().as_ref(), &FRAME);
        assert_eq!(peer.transport, TransportType::WebTransport);
        assert!(peer.mcp_host_key.is_some());
    }
}

/// Missing, unknown, mixed, duplicate, bare, and non-bearer carriers are
/// all refused before a session exists.
#[tokio::test]
async fn bad_token_carriers_are_refused() {
    let (_tokens, store) = token_store();
    let (_dir, listener, url) = listener(Some(store));
    let token = hex::encode(TEST_TOKEN);
    let wrong = hex::encode([0x22u8; crate::auth::TOKEN_LEN]);
    let refused = [
        ConnectOptions::builder(&url).build(),
        ConnectOptions::builder(format!("{url}?token={wrong}")).build(),
        ConnectOptions::builder(format!("{url}?token={token}"))
            .add_header("authorization", format!("Bearer {token}"))
            .build(),
        ConnectOptions::builder(format!("{url}?token={token}&token={token}")).build(),
        ConnectOptions::builder(format!("{url}?token={token}&token")).build(),
        ConnectOptions::builder(&url)
            .add_header("authorization", "Basic not-a-bearer")
            .build(),
    ];
    for options in refused {
        assert_refused(&listener, options).await;
    }
}

/// wtransport's client cannot emit repeated identical `authorization`
/// fields (its headers are a map), so speak raw HTTP/3 to send them and
/// assert the 403 comes before token verification would have succeeded.
#[tokio::test]
async fn raw_duplicate_authorization_is_refused_before_auth() {
    let (_tokens, store) = token_store();
    let (_dir, listener, _) = listener(Some(store));
    let addr = listener.local_addr().unwrap();
    let bearer = format!("Bearer {}", hex::encode(TEST_TOKEN));

    let client = async {
        let (status, conn) = raw_connect_status(
            addr,
            vec![
                (":method", "CONNECT"),
                (":scheme", "https"),
                (":protocol", "webtransport"),
                (":authority", "127.0.0.1"),
                (":path", "/session"),
                ("authorization", bearer.as_str()),
                ("authorization", bearer.as_str()),
            ],
        )
        .await;
        assert_eq!(status, 403);
        // A refused peer's connection is closed, not held open by the
        // listener's stream-drain task until the peer leaves.
        tokio::time::timeout(Duration::from_secs(5), conn.closed())
            .await
            .expect("the refused connection is closed");
    };
    tokio::select! {
        () = client => {}
        _ = listener.accept() => panic!("duplicate Authorization was accepted"),
    }
}

/// Drive a CONNECT whose QPACK payload is `headers` (preserving repeats)
/// and return the `:status` the listener answers with, and the
/// connection.
async fn raw_connect_status(
    addr: SocketAddr,
    headers: Vec<(&str, &str)>,
) -> (u16, quinn::Connection) {
    use std::borrow::Cow;
    use wtransport_proto::frame::Frame;
    use wtransport_proto::qpack::Encoder;

    let crypto =
        phux_dial::tls::client_config(&phux_dial::CertTrust::SkipVerify, Some(b"h3")).unwrap();
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
    )));
    let conn = endpoint
        .connect(addr, "localhost")
        .unwrap()
        .await
        .expect("QUIC handshake");
    let _settings = h3::send_local_settings(&conn)
        .await
        .expect("client SETTINGS");
    let (mut send, mut recv) = conn.open_bi().await.expect("CONNECT stream");
    let payload = Encoder::encode(headers);
    Frame::new_headers(Cow::Owned(payload.into_vec()))
        .write_async(&mut h3::H3Send(&mut send))
        .await
        .expect("CONNECT HEADERS");
    let frame = Frame::read_async(&mut h3::H3Recv(&mut recv))
        .await
        .expect("CONNECT response");
    let status = Decoder::decode(frame.payload())
        .expect("response QPACK")
        .get(":status")
        .expect("response :status")
        .parse()
        .expect("numeric :status");
    (status, conn)
}

#[tokio::test]
async fn no_stream_session_does_not_block_next_consumer() {
    let (_dir, listener, url) = listener(None);
    let clients = async {
        let stalled = client_endpoint().connect(&url).await.unwrap();
        let conn = client_endpoint().connect(&url).await.unwrap();
        let (mut send, _recv) = conn.open_bi().await.unwrap().await.unwrap();
        send.write_all(&FRAME).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(stalled);
    };
    let accepted = async {
        let (mut reader, _writer, _peer) = listener.accept().await.unwrap();
        reader.read_frame().await.unwrap().unwrap()
    };

    let ((), frame) = tokio::time::timeout(HANG_GUARD, async { tokio::join!(clients, accepted) })
        .await
        .expect("healthy consumer must not wait for stalled first stream");
    assert_eq!(frame.as_ref(), FRAME);
}
