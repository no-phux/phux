//! Workload identity through a relay (ADR-0154 item 5): the consumer runs
//! TLS end to end with the server inside the relayed stream, so its
//! certificate reaches the server although the relay terminates the outer
//! TLS. That session is the only way in, in every posture (ADR-0180).
//!
//! ```text
//! consumer ==TLS(relay pin)==> relay <== connector (in server)
//!          \_____ TLS(server CA pin, client cert) ______/
//! ```

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use phux_config::ConnectorConfigEntry;
use phux_dial::quic::{DialedStream, InnerTls};
use phux_dial::{CertTrust, DialError, QuicDial, TlsClientIdentity};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::wire::frame::FrameKind;
use phux_relay::{RelayConfig, cert_fingerprint};
use phux_server::workload::WorkloadPaths;
use phux_server::{ServerConfig, ServerRuntime};
use phux_server_testkit::relay::RelayHarness;
use phux_server_testkit::{WIRE_RECV_TIMEOUT, encode_frame};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};

const ROUTE: &str = "inner-test";
const TUNNEL_TOKEN: [u8; 32] = [0x31; 32];

struct Topology {
    _dir: TempDir,
    relay: RelayHarness,
    relay_pin: String,
    paths: WorkloadPaths,
    authority: String,
    consumer_token: Vec<u8>,
    stop_server: oneshot::Sender<()>,
    server: tokio::task::JoinHandle<Result<(), phux_server::ServerError>>,
}

fn write_token(path: &Path, token: &[u8]) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, format!("{}\n", hex::encode(token))).expect("write token");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("chmod token");
}

impl Topology {
    /// A relay, and behind it a server whose certificate its workload CA
    /// issued; `paired` puts the server in `paired` policy mode, and
    /// otherwise it runs an embedder engine (ADR-0072), the only way a
    /// connector serves without `paired` (ADR-0179).
    fn start(paired: bool) -> Self {
        let dir = TempDir::new().expect("tempdir");
        std::fs::set_permissions(
            dir.path(),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .expect("owner-only");
        let d = dir.path();
        std::fs::write(
            d.join("relay-tokens"),
            format!("{} {ROUTE}\n", hex::encode(TUNNEL_TOKEN)),
        )
        .expect("relay route");
        write_token(&d.join("connector-token"), &TUNNEL_TOKEN);
        let consumer_token = phux_server_testkit::embedder::mint_bearer(&d.join("consumer-tokens"));
        let paths = WorkloadPaths::with_overrides(
            Some(d.join("workload-ca.pem")),
            Some(d.join("workload-ca.key")),
            Some(d.join("workload-keys")),
        );
        let (cert, key) = (d.join("remote-cert.pem"), d.join("remote-key.pem"));
        phux_server::transport::tls::ensure_server_identity(&cert, &key, &[], &paths)
            .expect("server identity");
        let authority = phux_server::transport::tls::presented_authority(&cert)
            .expect("chain")
            .expect("issued by the CA");

        let relay = RelayHarness::start(RelayConfig {
            max_conns: 16,
            ..phux_relay::config_in(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0), d)
        });
        let relay_pin = cert_fingerprint(&d.join("relay-cert.pem")).expect("relay pin");
        let connector = ConnectorConfigEntry {
            relay: relay.addr.to_string(),
            token_file: Some(d.join("connector-token")),
            cert_fingerprint: Some(relay_pin.clone()),
        };
        let mut cfg = ServerConfig {
            socket_path: d.join("phux.sock"),
            policy_mode: paired.then_some(phux_config::PolicyMode::Paired),
            env: phux_server::ServerEnv {
                ws_tokens: Some(d.join("consumer-tokens")),
                tls_cert: Some(cert),
                tls_key: Some(key),
                // Naming the authority without `paired` refuses to start
                // (workload-auth §8), so only the paired server names it.
                workload_ca: paired.then(|| paths.ca_cert.clone()),
                workload_ca_key: paired.then(|| paths.ca_key.clone()),
                workload_keys: paired.then(|| paths.registry.clone()),
                ..phux_server::ServerEnv::default()
            },
            ..ServerConfig::with_default_socket()
        };
        if !paired {
            phux_server_testkit::embedder::embed_owner_engine(&mut cfg);
        }
        let (stop_server, stopped) = oneshot::channel();
        let server = tokio::task::spawn_local(async move {
            ServerRuntime::new(cfg)
                .connectors(vec![connector], None)
                .run_async(async move {
                    let _ = stopped.await;
                })
                .await
        });
        Self {
            _dir: dir,
            relay,
            relay_pin,
            paths,
            authority,
            consumer_token,
            stop_server,
            server,
        }
    }

    fn plan(&self, inner: Option<InnerTls>) -> QuicDial {
        QuicDial {
            addr: self.relay.addr,
            server_name: ROUTE.to_owned(),
            token: Some(self.consumer_token.clone()),
            trust: CertTrust::Pinned(self.relay_pin.clone()),
            identity: Some(TlsClientIdentity::None),
            inner,
        }
    }

    async fn stop(self) {
        let _ = self.stop_server.send(());
        self.server.await.expect("server task").expect("shutdown");
        self.relay.stop().await;
    }
}

/// The end-to-end session to the server behind [`ROUTE`], pinning `ca`.
fn inner(ca: &str, identity: TlsClientIdentity) -> InnerTls {
    InnerTls {
        trust: CertTrust::Authority { ca: ca.to_owned() },
        identity,
        server_name: ROUTE.to_owned(),
    }
}

/// HELLO through `plan`; the server's first answer, or why the dial failed.
async fn hello(plan: &QuicDial) -> Result<FrameKind, DialError> {
    let (_endpoint, conn, stream) = phux_dial::quic::dial_stream(plan).await?;
    let hello = encode_frame(&FrameKind::Hello {
        client_name: "relay-inner-tls".to_owned(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: ClientCapabilities::default(),
    });
    let io = async {
        let (mut reader, mut writer): (
            Box<dyn tokio::io::AsyncRead + Unpin>,
            Box<dyn tokio::io::AsyncWrite + Unpin>,
        ) = match stream {
            DialedStream::Plain { send, recv } => (Box::new(recv), Box::new(send)),
            DialedStream::Inner(stream) => {
                let (recv, send) = tokio::io::split(*stream);
                (Box::new(recv), Box::new(send))
            }
        };
        writer.write_all(&hello).await?;
        writer.flush().await?;
        let mut header = [0_u8; 4];
        reader.read_exact(&mut header).await?;
        let mut framed = header.to_vec();
        framed.resize(4 + u32::from_be_bytes(header) as usize, 0);
        reader.read_exact(&mut framed[4..]).await?;
        Ok::<_, std::io::Error>(framed)
    };
    let framed = timeout(WIRE_RECV_TIMEOUT, io)
        .await
        .map_err(|_| DialError::Connect("no answer".to_owned()))?
        .map_err(|err| {
            conn.close_reason().map_or_else(
                || DialError::Connect(err.to_string()),
                |close| phux_dial::quic::close_error(&close),
            )
        })?;
    let (frame, _) =
        FrameKind::decode(&framed).map_err(|err| DialError::Connect(err.to_string()))?;
    Ok(frame)
}

/// Retry until the connector holds the route.
async fn first_answer(plan: &QuicDial) -> Result<FrameKind, DialError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let answer = hello(plan).await;
        let route_offline =
            matches!(&answer, Err(DialError::Connect(detail)) if detail.contains("route offline"));
        if !route_offline || Instant::now() > deadline {
            return answer;
        }
        sleep(Duration::from_millis(100)).await;
    }
}

/// The connector's first-byte gate refused a plain relayed stream: the
/// route was up, the stream reached the server, and the server reset it on
/// the tunnel with `AUTH_FAILED`. The relay forwards no reset code (it only
/// splices bytes), so the consumer sees the relay end its bridged
/// connection with code 0 (or, should the stream's FIN be read before that
/// close lands, the stream end), after the dial succeeded and before any
/// answer. Not a timeout ("no answer"), not `ROUTE_OFFLINE`, not a dial
/// failure.
fn assert_plain_refused(answer: &Result<FrameKind, DialError>) {
    assert!(
        matches!(
            answer,
            Err(DialError::Connect(detail))
                if detail == "closed by peer: 0" || detail == "early eof"
        ),
        "a plain bridged consumer must be refused by the connector gate, got {answer:?}"
    );
}

/// Wait until the server's connector holds the route.
async fn wait_for_route(t: &Topology) {
    let answer = first_answer(&t.plan(None)).await;
    assert!(
        !matches!(&answer, Err(DialError::Connect(detail)) if detail.contains("route offline")),
        "the connector never held the route: {answer:?}"
    );
}

fn ticket(paths: &WorkloadPaths) -> Vec<u8> {
    let minted = phux_server::workload::tickets::mint_ticket(
        &phux_server::workload::tickets::tickets_path(&paths.registry),
        vec!["inventory,observe,create,bind,input,signal@global".to_owned()],
        3600,
        600,
    )
    .expect("ticket");
    hex::decode(minted.secret_hex).expect("hex")
}

/// Even under an embedder engine, which admits a bearer alone, the plain
/// relayed stream is refused (ADR-0180: the relay would read it); an inner
/// session pinning the server's CA is admitted, and a wrong CA is refused by
/// name.
#[test]
fn an_inner_session_reaches_the_server_and_a_wrong_ca_is_refused() {
    phux_server_testkit::run_local(async {
        let t = Topology::start(false);
        wait_for_route(&t).await;
        // In every posture.
        assert_plain_refused(&first_answer(&t.plan(None)).await);
        let session = inner(&t.authority, TlsClientIdentity::None);
        let answer = first_answer(&t.plan(Some(session)))
            .await
            .expect("inner admitted");
        assert!(matches!(answer, FrameKind::HelloOk { .. }), "{answer:?}");

        let wrong = inner(
            &format!("sha256:{}", "0".repeat(64)),
            TlsClientIdentity::None,
        );
        match first_answer(&t.plan(Some(wrong))).await {
            Err(DialError::AuthorityChanged(change)) => {
                assert_eq!(change.presented.as_deref(), Some(t.authority.as_str()));
            }
            other => panic!("a wrong CA must be refused by name, got {other:?}"),
        }
        t.stop().await;
    });
}

/// Under `paired`: a plain consumer and a certificate-less inner one are
/// refused; a device enrolls through the relay with a ticket and is then
/// admitted with that certificate; the ticket does not enroll twice.
#[test]
fn under_paired_only_an_enrolled_certificate_crosses_the_relay() {
    phux_server_testkit::run_local(async {
        let t = Topology::start(true);
        wait_for_route(&t).await;
        assert_plain_refused(&first_answer(&t.plan(None)).await);
        let bare = inner(&t.authority, TlsClientIdentity::None);
        assert!(
            !matches!(
                first_answer(&t.plan(Some(bare))).await,
                Ok(FrameKind::HelloOk { .. })
            ),
            "an inner session without a certificate is refused"
        );

        let key = rcgen::KeyPair::generate().expect("key");
        let csr = rcgen::CertificateParams::new(Vec::<String>::new())
            .expect("params")
            .serialize_request(&key)
            .expect("csr")
            .der()
            .to_vec();
        let ticket = ticket(&t.paths);
        let enroll_dial = phux_dial::enroll::EnrollDial {
            addr: t.relay.addr,
            server_name: ROUTE.to_owned(),
            trust: CertTrust::Pinned(t.relay_pin.clone()),
            inner: Some(inner(&t.authority, TlsClientIdentity::None)),
        };
        let request = phux_protocol::enroll::Request {
            ticket: ticket.clone(),
            csr: csr.clone(),
        };
        let chain = phux_dial::enroll::enroll(&enroll_dial, &request)
            .await
            .expect("enrolled through the relay");
        phux_dial::enroll::check_issued_chain(
            &chain,
            &rcgen::PublicKeyData::subject_public_key_info(&key),
            Some(&t.authority),
        )
        .expect("the reply passes the client checks");
        let replay = phux_dial::enroll::enroll(&enroll_dial, &request).await;
        assert!(
            matches!(replay, Err(DialError::AuthRefused(_))),
            "{replay:?}"
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let (cert, key_path) = (dir.path().join("c.pem"), dir.path().join("c.key"));
        std::fs::write(&cert, &chain).expect("chain");
        write_key(&key_path, &key.serialize_pem());
        let enrolled = inner(
            &t.authority,
            TlsClientIdentity::PemFiles {
                certificate: cert,
                private_key: key_path,
            },
        );
        let answer = first_answer(&t.plan(Some(enrolled)))
            .await
            .expect("admitted");
        assert!(matches!(answer, FrameKind::HelloOk { .. }), "{answer:?}");
        t.stop().await;
    });
}

fn write_key(path: &PathBuf, pem: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, pem).expect("key");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
}
