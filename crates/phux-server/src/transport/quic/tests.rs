//! QUIC transport tests: admission, the preamble, and workload certificates.

use super::*;

use super::super::tls::{QUIC_ALPN, ensure_self_signed};
use phux_protocol::ids::ResourceId;

const TEST_TOKEN: [u8; crate::auth::TOKEN_LEN] = [0x11; crate::auth::TOKEN_LEN];
/// One complete framed message: 4-byte length prefix (body = 3) + body.
const FRAME: [u8; 7] = [0, 0, 0, 3, 0xde, 0xad, 0xbe];

async fn join_bounded<S: Future, C: Future>(server: S, client: C) -> (S::Output, C::Output) {
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(server, client)
    })
    .await
    .expect("paired QUIC test completes")
}

fn focus_frame() -> BytesMut {
    let mut frame = BytesMut::new();
    phux_protocol::wire::frame::FrameKind::InputFocus {
        terminal_id: ResourceId::local(7),
        event: phux_protocol::input::focus::FocusEvent::Gained,
    }
    .encode(&mut frame);
    frame
}

/// A self-signed cert + key in a fresh tempdir, kept alive for the test.
fn cert_pair() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    ensure_self_signed(&cert, &key).unwrap();
    (dir, cert, key)
}

/// A loopback listener; `tokens` requires a pairing-store preamble.
fn listener(
    tokens: Option<Arc<crate::auth::ReloadingTokenStore>>,
) -> (tempfile::TempDir, QuicListener, SocketAddr) {
    let (dir, cert, key) = cert_pair();
    let listener = QuicListener::from_pem_with_client_ca_and_registry(
        "127.0.0.1:0".parse().unwrap(),
        &cert,
        &key,
        tokens,
        None,
        None,
        None,
    )
    .unwrap();
    let addr = listener.local_addr().unwrap();
    (dir, listener, addr)
}

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

/// A client endpoint offering the phux ALPN, trusting any certificate
/// through the same builder production dialers use.
fn client_endpoint_with(transport: Option<quinn::TransportConfig>) -> quinn::Endpoint {
    let crypto =
        phux_dial::tls::client_config(&phux_dial::CertTrust::SkipVerify, Some(QUIC_ALPN)).unwrap();
    let mut config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
    ));
    if let Some(transport) = transport {
        config.transport_config(Arc::new(transport));
    }
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(config);
    endpoint
}

fn client_endpoint() -> quinn::Endpoint {
    client_endpoint_with(None)
}

/// Connect to `addr` and open the control stream.
async fn open_control(
    endpoint: &quinn::Endpoint,
    addr: SocketAddr,
) -> (quinn::Connection, quinn::SendStream, quinn::RecvStream) {
    let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
    let (send, recv) = conn.open_bi().await.unwrap();
    (conn, send, recv)
}

/// Frame a token as the auth preamble: `len: u32 BE` + token bytes.
fn token_preamble(token: &[u8]) -> Vec<u8> {
    let mut buf = (u32::try_from(token.len()).unwrap()).to_be_bytes().to_vec();
    buf.extend_from_slice(token);
    buf
}

fn stream_bind_bytes(terminal: u32, stream: u64) -> Vec<u8> {
    use phux_protocol::ids::StreamId;
    use phux_protocol::wire::stream_bind::{StreamBind, encode};
    let mut buf = BytesMut::new();
    encode(
        &StreamBind {
            terminal_id: ResourceId::local(terminal),
            stream_id: StreamId::new(stream).unwrap(),
        },
        &mut buf,
    );
    buf.to_vec()
}

/// Start the admitted pump for a `Bound` event, as the client task does.
/// Returns the bound ids, the Terminal byte budget, and the liveness flag.
fn spawn_pump(
    event: QuicStreamEvent,
) -> (
    ResourceId,
    phux_protocol::ids::StreamId,
    Arc<tokio::sync::Semaphore>,
    Arc<AtomicBool>,
) {
    let QuicStreamEvent::Bound {
        terminal_id,
        stream_id,
        recv,
        frames,
        frame_bytes,
        terminal_frame_bytes,
        ..
    } = event
    else {
        panic!("expected Bound, got {event:?}");
    };
    let active = Arc::new(AtomicBool::new(true));
    tokio::spawn(pump_terminal_stream(
        recv,
        terminal_id.clone(),
        stream_id,
        frames,
        Arc::clone(&frame_bytes),
        Arc::clone(&terminal_frame_bytes),
        Arc::clone(&active),
        tokio_util::sync::CancellationToken::new(),
        None,
    ));
    (terminal_id, stream_id, terminal_frame_bytes, active)
}

#[tokio::test]
async fn partial_frames_are_typed_as_truncation() {
    for bytes in [&[0, 0][..], &[0, 0, 0, 3, 0xde]] {
        let (_dir, listener, addr) = listener(None);
        let server = async {
            let (mut reader, _writer, _) = listener.accept().await.unwrap();
            let err = reader.read_frame().await.expect_err("truncation");
            assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "{bytes:?}");
        };
        let client = async {
            let (_conn, mut send, _recv) = open_control(&client_endpoint(), addr).await;
            send.write_all(bytes).await.unwrap();
            send.finish().unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        join_bounded(server, client).await;
    }
}

/// Loopback admits without a preamble and stamps no device id; a paired
/// listener authenticates the preamble and stamps one.
#[tokio::test]
async fn round_trips_a_frame_open_and_authenticated() {
    let (_tokens, store) = token_store();
    for tokens in [None, Some(store)] {
        let authenticated = tokens.is_some();
        let (_dir, listener, addr) = listener(tokens);
        let server = async {
            let (mut reader, _writer, peer) = listener.accept().await.unwrap();
            (reader.read_frame().await.unwrap(), peer)
        };
        let client = async {
            let (_conn, mut send, _recv) = open_control(&client_endpoint(), addr).await;
            if authenticated {
                send.write_all(&token_preamble(&TEST_TOKEN)).await.unwrap();
            }
            send.write_all(&FRAME).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let ((got, peer), ()) = join_bounded(server, client).await;
        assert_eq!(got.unwrap().as_ref(), &FRAME);
        assert_eq!(peer.transport, TransportType::Quic);
        assert_eq!(peer.mcp_host_key.is_some(), authenticated);
    }
}

/// A peer that completes the handshake and then says nothing holds one
/// admission slot, not the listener: a healthy peer behind it is
/// admitted at once instead of after the silent one's deadline.
#[tokio::test]
async fn a_silent_connection_does_not_delay_other_admissions() {
    let (_tokens, store) = token_store();
    let (_dir, listener, addr) = listener(Some(store));
    let silent_endpoint = client_endpoint();
    let healthy_endpoint = client_endpoint();
    let server = async {
        let (mut reader, _writer, _peer) = listener.accept().await.unwrap();
        reader.read_frame().await.unwrap()
    };
    let client = async {
        let silent = silent_endpoint
            .connect(addr, "localhost")
            .unwrap()
            .await
            .unwrap();
        let (_conn, mut send, _recv) = open_control(&healthy_endpoint, addr).await;
        send.write_all(&token_preamble(&TEST_TOKEN)).await.unwrap();
        send.write_all(&FRAME).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        silent
    };
    let (got, _silent) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(server, client)
    })
    .await
    .expect("the healthy peer is admitted without waiting on the silent one");
    assert_eq!(got.unwrap().as_ref(), &FRAME);
}

#[tokio::test]
async fn invalid_token_preamble_is_refused() {
    let (_tokens, store) = token_store();
    let (_dir, listener, addr) = listener(Some(store));
    // The listener loops on refusals, so assert from the client side
    // while it runs.
    let client = async {
        let (conn, mut send, _recv) = open_control(&client_endpoint(), addr).await;
        let _ = send
            .write_all(&token_preamble(&[0x22u8; crate::auth::TOKEN_LEN]))
            .await;
        tokio::time::timeout(Duration::from_secs(5), conn.closed())
            .await
            .expect("server must refuse promptly, not hang")
    };
    let closed = tokio::select! {
        closed = client => closed,
        _ = listener.accept() => panic!("a refused peer was admitted"),
    };
    match closed {
        quinn::ConnectionError::ApplicationClosed(close) => {
            assert_eq!(u64::from(AUTH_FAILED_CODE), close.error_code.into_inner());
        }
        other => panic!("expected application close on auth failure, got {other:?}"),
    }
}

/// A CSR-enrolled workload client: its identity files and credential.
fn enrolled_client(
    paths: &crate::workload::WorkloadPaths,
    dir: &std::path::Path,
) -> (
    phux_dial::TlsClientIdentity,
    crate::workload::RegisteredCredential,
) {
    let client_key = rcgen::KeyPair::generate().unwrap();
    let csr = rcgen::CertificateParams::new(vec!["client".to_owned()])
        .unwrap()
        .serialize_request(&client_key)
        .unwrap();
    let material =
        crate::workload::ClientMaterial::from_pem(csr.pem().unwrap().as_bytes()).unwrap();
    let expires = chrono::Utc::now().timestamp() + 3600;
    let prepared = crate::workload::prepare_enrollment(paths, &material, expires).unwrap();
    let certificate = dir.join("client.pem");
    let private_key = dir.join("client.key");
    std::fs::write(&certificate, prepared.issued_chain_pem().unwrap()).unwrap();
    std::fs::write(&private_key, client_key.serialize_pem()).unwrap();
    let registered = prepared
        .commit(&paths.registry, vec!["*@global".to_owned()], expires)
        .unwrap();
    (
        phux_dial::TlsClientIdentity::PemFiles {
            certificate,
            private_key,
        },
        registered,
    )
}

/// The `OPEN_LISTENER` door in workload mode keeps its one-attach token
/// as outer admission, refuses a dial without a client certificate, and
/// stamps an enrolled certificate's credential.
#[tokio::test]
async fn an_ephemeral_listener_in_workload_mode_requires_an_enrolled_certificate() {
    let (dir, cert, key) = cert_pair();
    let paths = crate::workload::WorkloadPaths {
        ca_cert: dir.path().join("ca.pem"),
        ca_key: dir.path().join("ca.key"),
        registry: dir.path().join("workload-keys"),
    };
    crate::workload::init_authority(&paths.ca_cert, &paths.ca_key).unwrap();
    let (identity, enrolled) = enrolled_client(&paths, dir.path());
    let ca_certificate = crate::workload::authority_certificate(&paths.ca_cert).unwrap();
    let (token, secret) = crate::auth::ListenerToken::mint().unwrap();
    let token = Arc::new(token);
    let door = || {
        let registry = Arc::new(
            crate::workload::ReloadingWorkloadRegistry::load(paths.registry.clone()).unwrap(),
        );
        QuicListener::with_admission(
            "127.0.0.1:0".parse().unwrap(),
            &cert,
            &key,
            QuicAdmission::Listener(Arc::clone(&token)),
            Some((&ca_certificate, registry)),
        )
        .unwrap()
    };

    let refusing = door();
    let refusing_addr = refusing.local_addr().unwrap();
    let anonymous = client_endpoint();
    let refused = async {
        let conn = anonymous
            .connect(refusing_addr, "localhost")
            .ok()?
            .await
            .ok()?;
        let (mut send, mut recv) = conn.open_bi().await.ok()?;
        send.write_all(&token_preamble(&secret)).await.ok()?;
        let mut byte = [0u8; 1];
        recv.read_exact(&mut byte).await.ok()
    };
    let refused = tokio::select! {
        refused = tokio::time::timeout(Duration::from_secs(10), refused) => refused,
        _ = refusing.accept() => panic!("a dial without a certificate was admitted"),
    };
    assert!(
        refused.expect("a refusal settles").is_none(),
        "a dial without a workload certificate is refused"
    );

    let listener = door();
    let addr = listener.local_addr().unwrap();
    let crypto = phux_dial::tls::client_config_with_identity(
        &phux_dial::CertTrust::SkipVerify,
        &identity,
        Some(QUIC_ALPN),
    )
    .unwrap();
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
    )));
    let server = async {
        let (mut reader, _writer, peer) = listener.accept().await.unwrap();
        (reader.read_frame().await.unwrap(), peer)
    };
    let client = async {
        let (_conn, mut send, _recv) = open_control(&endpoint, addr).await;
        send.write_all(&token_preamble(&secret)).await.unwrap();
        send.write_all(&FRAME).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let ((got, peer), ()) = join_bounded(server, client).await;
    assert_eq!(got.unwrap().as_ref(), &FRAME);
    let credential = peer.credential.as_ref().expect("the workload credential");
    assert_eq!(credential.id, enrolled.id);
    assert_eq!(credential.generation, enrolled.generation);
    assert!(credential.registry_instance.is_some());
}

mod mux;
