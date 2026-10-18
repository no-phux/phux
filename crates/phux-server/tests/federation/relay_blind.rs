//! The relay is blind (ADR-0180): a relayed attach works end to end while
//! the production relay, tapped for every byte it forwards, sees only the
//! consumer's end-to-end TLS session with the server. A relay operator that
//! swaps in its own server behind the route cannot finish that session, and
//! the consumer's bearer never crosses.
//!
//! ```text
//! consumer --QUIC(relay pin)--> relay [tap] <--"phux-relay/1"-- connector (in server)
//!          \__________ TLS 1.3 (server CA pin), terminated by the server __________/
//! ```

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use phux_config::ConnectorConfigEntry;
use phux_dial::quic::{DialedStream, InnerStream, InnerTls};
use phux_dial::{CertTrust, DialError, QuicDial, TlsClientIdentity};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::FrameKind;
use phux_relay::{RelayConfig, cert_fingerprint};
use phux_server::workload::WorkloadPaths;
use phux_server::{ServerConfig, ServerRuntime};
use phux_server_testkit::relay::RelayHarness;
use phux_server_testkit::{WIRE_RECV_TIMEOUT, attach_by_name, encode_frame, seed_pty};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};

const ROUTE: &str = "blind-route";
const TUNNEL_TOKEN: [u8; 32] = [0x51; 32];
/// Typed into the pane and echoed back by `cat`: it crosses the relay once
/// in each direction, as input and as terminal output.
const SECRET: &str = "zebrafish-keystrokes-the-relay-must-not-see";
/// The consumer's HELLO names itself with this.
const CLIENT_NAME: &str = "blind-relay-consumer-hello";

fn write_secret(path: &Path, token: &[u8]) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, format!("{}\n", hex::encode(token))).expect("write token");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("chmod token");
}

/// A server identity issued by its own workload CA under `dir`: the
/// certificate, its key, and the CA's `sha256:` pin.
fn server_identity(dir: &Path) -> (PathBuf, PathBuf, String) {
    std::fs::create_dir_all(dir).expect("identity dir");
    let paths = WorkloadPaths::with_overrides(
        Some(dir.join("workload-ca.pem")),
        Some(dir.join("workload-ca.key")),
        Some(dir.join("workload-keys")),
    );
    let (cert, key) = (dir.join("remote-cert.pem"), dir.join("remote-key.pem"));
    phux_server::transport::tls::ensure_server_identity(&cert, &key, &[], &paths)
        .expect("server identity");
    let authority = phux_server::transport::tls::presented_authority(&cert)
        .expect("chain")
        .expect("issued by the CA");
    (cert, key, authority)
}

/// One server behind the route: its own identity and token store under
/// `dir`, its connector holding the route on `relay`.
struct Server {
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Result<(), phux_server::ServerError>>,
}

impl Server {
    fn start(
        dir: &Path,
        relay: SocketAddr,
        relay_pin: &str,
        seed: bool,
    ) -> (Self, String, Vec<u8>) {
        let (cert, key, authority) = server_identity(dir);
        write_secret(&dir.join("connector-token"), &TUNNEL_TOKEN);
        let bearer = phux_server_testkit::embedder::mint_bearer(&dir.join("consumer-tokens"));
        let connector = ConnectorConfigEntry {
            relay: relay.to_string(),
            token_file: Some(dir.join("connector-token")),
            cert_fingerprint: Some(relay_pin.to_owned()),
        };
        let mut cfg = ServerConfig {
            socket_path: dir.join("phux.sock"),
            pre_seeded_session: seed.then(|| "default".to_owned()),
            seed_with_pty: false,
            seed_command: None,
            env: phux_server::ServerEnv {
                ws_tokens: Some(dir.join("consumer-tokens")),
                tls_cert: Some(cert),
                tls_key: Some(key),
                ..phux_server::ServerEnv::default()
            },
            ..ServerConfig::with_default_socket()
        };
        if seed {
            seed_pty(&mut cfg, CommandBuilder::new("/bin/cat"));
        }
        // The relay's view does not depend on who may act; `relay_inner_tls`
        // covers `paired`.
        phux_server_testkit::embedder::embed_owner_engine(&mut cfg);
        let (stop, stopped) = oneshot::channel();
        let task = tokio::task::spawn_local(async move {
            ServerRuntime::new(cfg)
                .connectors(vec![connector], None)
                .run_async(async move {
                    let _ = stopped.await;
                })
                .await
        });
        (Self { stop, task }, authority, bearer)
    }

    async fn stop(self) {
        let _ = self.stop.send(());
        self.task.await.expect("server task").expect("shutdown");
    }
}

/// A tapped relay: everything it forwards for a consumer lands in `tap`.
struct Relay {
    harness: RelayHarness,
    pin: String,
    tap: Arc<Mutex<Vec<u8>>>,
}

impl Relay {
    fn start(dir: &Path) -> Self {
        std::fs::write(
            dir.join("relay-tokens"),
            format!("{} {ROUTE}\n", hex::encode(TUNNEL_TOKEN)),
        )
        .expect("relay route");
        let tap = Arc::new(Mutex::new(Vec::new()));
        let harness = RelayHarness::start_tapped(
            RelayConfig {
                max_conns: 16,
                ..phux_relay::config_in(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0), dir)
            },
            Arc::clone(&tap),
        );
        let pin = cert_fingerprint(&dir.join("relay-cert.pem")).expect("relay pin");
        Self { harness, pin, tap }
    }

    fn forwarded(&self) -> Vec<u8> {
        self.tap.lock().expect("tap").clone()
    }

    /// A consumer plan through this relay to the server whose CA is `ca`.
    fn plan(&self, ca: &str, bearer: &[u8]) -> QuicDial {
        QuicDial {
            addr: self.harness.addr,
            server_name: ROUTE.to_owned(),
            token: Some(bearer.to_vec()),
            trust: CertTrust::Pinned(self.pin.clone()),
            identity: Some(TlsClientIdentity::None),
            inner: Some(InnerTls {
                trust: CertTrust::Authority { ca: ca.to_owned() },
                identity: TlsClientIdentity::None,
                server_name: ROUTE.to_owned(),
            }),
        }
    }
}

/// The end-to-end session, retried while the connector has not yet claimed
/// the route.
async fn session(plan: &QuicDial) -> Result<(quinn::Endpoint, InnerStream), DialError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match phux_dial::quic::dial_stream(plan).await {
            Ok((endpoint, _conn, DialedStream::Inner(stream))) => return Ok((endpoint, *stream)),
            Ok((_, _, DialedStream::Plain { .. })) => panic!("a relayed dial is never plain"),
            Err(DialError::Connect(detail))
                if detail.contains("route offline") && Instant::now() < deadline =>
            {
                sleep(Duration::from_millis(100)).await;
            }
            Err(err) => return Err(err),
        }
    }
}

async fn recv(stream: &mut InnerStream) -> FrameKind {
    timeout(WIRE_RECV_TIMEOUT, async {
        let mut header = [0_u8; 4];
        stream.read_exact(&mut header).await.expect("frame header");
        let mut framed = header.to_vec();
        framed.resize(4 + u32::from_be_bytes(header) as usize, 0);
        stream
            .read_exact(&mut framed[4..])
            .await
            .expect("frame body");
        FrameKind::decode(&framed).expect("frame").0
    })
    .await
    .expect("frame within the deadline")
}

async fn send(stream: &mut InnerStream, frame: &FrameKind) {
    stream.write_all(&encode_frame(frame)).await.expect("send");
    stream.flush().await.expect("flush");
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// A relayed attach types into a real PTY and reads the echo, end to end,
/// while every byte the relay forwarded is ciphertext: no keystroke, no
/// terminal output, no HELLO, no bearer. What it does see is named: the
/// inner `ClientHello`, which carries the route as its server name.
#[test]
fn a_relayed_attach_works_and_the_relay_forwards_only_ciphertext() {
    let dir = TempDir::new().expect("tempdir");
    phux_server_testkit::run_local(async {
        let relay = Relay::start(dir.path());
        let (server, authority, bearer) = Server::start(
            &dir.path().join("server"),
            relay.harness.addr,
            &relay.pin,
            true,
        );

        let (_endpoint, mut stream) = session(&relay.plan(&authority, &bearer))
            .await
            .expect("the end-to-end session to the server");
        send(
            &mut stream,
            &FrameKind::Hello {
                client_name: CLIENT_NAME.to_owned(),
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                client_caps: ClientCapabilities::default(),
            },
        )
        .await;
        send(&mut stream, &attach_by_name("default")).await;
        let mut pane = None;
        let mut ready = false;
        while pane.is_none() || !ready {
            match recv(&mut stream).await {
                FrameKind::Attached { snapshot, .. } => {
                    pane = Some(snapshot.resources[0].id.clone());
                }
                FrameKind::BootstrapReady { .. } => ready = true,
                _ => {}
            }
        }
        send(
            &mut stream,
            &FrameKind::InputPaste {
                terminal_id: pane.expect("one pane"),
                event: PasteEvent {
                    trust: PasteTrust::Trusted,
                    data: format!("{SECRET}\r").into_bytes(),
                },
            },
        )
        .await;
        let mut echoed = Vec::new();
        while !contains(&echoed, SECRET.as_bytes()) {
            if let FrameKind::ResourceOutput { bytes, .. } = recv(&mut stream).await {
                echoed.extend_from_slice(&bytes);
            }
        }

        let forwarded = relay.forwarded();
        assert!(
            forwarded.len() > SECRET.len() * 2,
            "the relay forwarded the session: {} bytes",
            forwarded.len()
        );
        assert_eq!(
            forwarded.first(),
            Some(&0x16),
            "the first byte a consumer sends is a TLS handshake record"
        );
        for (what, needle) in [
            ("the keystrokes and their echo", SECRET.as_bytes()),
            ("the HELLO", CLIENT_NAME.as_bytes()),
            ("the bearer", bearer.as_slice()),
            ("the bearer, hex", hex::encode(&bearer).as_bytes()),
        ] {
            assert!(
                !contains(&forwarded, needle),
                "the relay saw {what} in the clear"
            );
        }
        // Honest metadata: the inner `ClientHello` is plaintext, and its
        // server name is the route the relay already routes on.
        assert!(contains(&forwarded, ROUTE.as_bytes()));

        server.stop().await;
        relay.harness.stop().await;
    });
}

/// A relay operator that puts its own server behind the route (it controls
/// the route table) cannot complete the consumer's session: the consumer
/// pins the real server's CA, refuses the impostor's by name, and never
/// writes its bearer, so neither the relay nor the impostor learns it.
#[test]
fn an_impostor_behind_the_route_fails_closed_and_never_sees_the_bearer() {
    let dir = TempDir::new().expect("tempdir");
    phux_server_testkit::run_local(async {
        let relay = Relay::start(dir.path());
        let (real, authority, bearer) = Server::start(
            &dir.path().join("real"),
            relay.harness.addr,
            &relay.pin,
            false,
        );
        // The real server holds the route and answers.
        let (_endpoint, mut stream) = session(&relay.plan(&authority, &bearer))
            .await
            .expect("the real server");
        send(&mut stream, &FrameKind::Ping { nonce: 7 }).await;
        assert!(matches!(
            recv(&mut stream).await,
            FrameKind::Pong { nonce: 7 }
        ));
        drop(stream);
        real.stop().await;

        // The operator's own server claims the same route with the same
        // tunnel token, presenting a certificate its own CA issued.
        let (impostor, impostor_ca, _) = Server::start(
            &dir.path().join("impostor"),
            relay.harness.addr,
            &relay.pin,
            false,
        );
        assert_ne!(impostor_ca, authority);
        let before = relay.forwarded().len();
        match session(&relay.plan(&authority, &bearer)).await {
            Err(DialError::AuthorityChanged(change)) => {
                assert_eq!(change.presented.as_deref(), Some(impostor_ca.as_str()));
            }
            Err(other) => panic!("the impostor must be refused by name, got {other}"),
            Ok(_) => panic!("a session to a server the consumer did not pin"),
        }
        let after = relay.forwarded();
        assert!(
            after.len() > before,
            "the impostor's handshake crossed the relay"
        );
        assert!(!contains(&after, &bearer), "the bearer crossed the relay");
        assert!(!contains(&after, hex::encode(&bearer).as_bytes()));

        impostor.stop().await;
        relay.harness.stop().await;
    });
}
