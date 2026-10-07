//! Dial-out relay end to end (ADR-0051/0052/0053): a server whose only
//! listener is a UDS in a tempdir dials OUT to the production `RelayRuntime`
//! through its production connector, and consumers reach it through the relay
//! with the production QUIC dialer.
//!
//! ```text
//! consumer --QUIC "phux-quic/1"--> relay <--"phux-relay/1"-- connector (in server) --> dispatch
//! ```

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::{Duration, Instant};

use phux_config::ConnectorConfigEntry;
use phux_dial::{CertTrust, QuicDial};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::frame::FrameKind;
use phux_relay::{RelayConfig, cert_fingerprint};
use phux_server::{ServerConfig, ServerRuntime};
use phux_server_testkit::relay::RelayHarness;
use phux_server_testkit::{WIRE_RECV_TIMEOUT, ascii_key, attach_by_name, encode_frame, seed_pty};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};

const ROUTE: &str = "connector-test";
const PING_NONCE: u64 = 0xC011_EC70;
const TUNNEL_TOKEN: [u8; 32] = [0x11; 32];
const CONSUMER_TOKEN: [u8; 32] = [0x22; 32];

/// A relay and a connector-configured server behind it, sharing a tempdir.
struct Topology {
    dir: TempDir,
    relay: Option<RelayHarness>,
    addr: SocketAddr,
    fingerprint: String,
    stop_server: oneshot::Sender<()>,
    server: tokio::task::JoinHandle<Result<(), phux_server::ServerError>>,
}

fn relay_config(dir: &Path, listen: SocketAddr) -> RelayConfig {
    RelayConfig {
        listen,
        cert_path: dir.join("relay-cert.pem"),
        key_path: dir.join("relay-key.pem"),
        tokens_path: dir.join("relay-tokens"),
        max_conns: 16,
    }
}

fn write_token(path: &Path, token: &[u8]) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, format!("{}\n", hex::encode(token))).expect("write token");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("chmod token");
}

/// Enroll the route and the consumer credential; [`Topology::start`] points
/// the server's token store at the consumer store.
fn provision(dir: &Path) {
    std::fs::write(
        dir.join("relay-tokens"),
        format!("{} {ROUTE}\n", hex::encode(TUNNEL_TOKEN)),
    )
    .expect("write relay route");
    write_token(&dir.join("connector-token"), &TUNNEL_TOKEN);
    let consumer_tokens = dir.join("consumer-tokens");
    write_token(&consumer_tokens, &CONSUMER_TOKEN);
    phux_server::auth::migrate_legacy_store(&consumer_tokens).expect("migrate consumer store");
}

impl Topology {
    /// Start the relay, then a server (optionally seeding session `default`
    /// with a PTY running `seed`) whose connector dials it.
    fn start(dir: TempDir, seed: Option<CommandBuilder>) -> Self {
        let relay = RelayHarness::start(relay_config(
            dir.path(),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        ));
        let fingerprint =
            cert_fingerprint(&dir.path().join("relay-cert.pem")).expect("relay fingerprint");
        let connector = ConnectorConfigEntry {
            relay: relay.addr.to_string(),
            token_file: Some(dir.path().join("connector-token")),
            cert_fingerprint: Some(fingerprint.clone()),
        };
        let mut cfg = ServerConfig {
            socket_path: dir.path().join("phux.sock"),
            pre_seeded_session: seed.is_some().then(|| "default".to_owned()),
            seed_with_pty: false,
            seed_command: None,
            env: phux_server::ServerEnv {
                ws_tokens: Some(dir.path().join("consumer-tokens")),
                ..phux_server::ServerEnv::default()
            },
            ..ServerConfig::with_default_socket()
        };
        if let Some(seed) = seed {
            seed_pty(&mut cfg, seed);
        }
        let (stop_server, server_stopped) = oneshot::channel();
        let server = tokio::task::spawn_local(async move {
            ServerRuntime::new(cfg)
                .connectors(vec![connector], None)
                .run_async(async move {
                    let _ = server_stopped.await;
                })
                .await
        });
        Self {
            dir,
            addr: relay.addr,
            relay: Some(relay),
            fingerprint,
            stop_server,
            server,
        }
    }

    async fn stop(self) {
        let _ = self.stop_server.send(());
        self.server
            .await
            .expect("server task")
            .expect("server shutdown");
        self.relay.expect("relay running").stop().await;
    }

    /// Stop the relay and start a fresh one on the same address.
    async fn restart_relay(&mut self) {
        self.relay.take().expect("relay running").stop().await;
        let restarted = RelayHarness::start(relay_config(self.dir.path(), self.addr));
        assert_eq!(restarted.addr, self.addr);
        self.relay = Some(restarted);
    }
}

/// A consumer stream through the relay.
struct Consumer {
    _endpoint: quinn::Endpoint,
    conn: quinn::Connection,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
}

async fn dial(
    addr: SocketAddr,
    fingerprint: &str,
    route: &str,
    token: Option<Vec<u8>>,
) -> Result<Consumer, String> {
    let dial = QuicDial {
        addr,
        server_name: route.to_owned(),
        token,
        trust: CertTrust::Pinned(fingerprint.to_owned()),
        identity: None,
        inner: None,
    };
    let (endpoint, conn, send, recv) = phux_dial::quic::dial(&dial)
        .await
        .map_err(|err| err.to_string())?;
    Ok(Consumer {
        _endpoint: endpoint,
        conn,
        send,
        recv,
    })
}

impl Consumer {
    async fn send(&mut self, frame: &FrameKind) -> Result<(), String> {
        self.send
            .write_all(&encode_frame(frame))
            .await
            .map_err(|err| err.to_string())
    }

    /// Read one frame; full consumption proves framing survived both hops.
    async fn recv(&mut self) -> Result<FrameKind, String> {
        timeout(WIRE_RECV_TIMEOUT, async {
            let mut header = [0u8; 4];
            self.recv
                .read_exact(&mut header)
                .await
                .map_err(|e| e.to_string())?;
            let mut framed = header.to_vec();
            framed.resize(4 + u32::from_be_bytes(header) as usize, 0);
            self.recv
                .read_exact(&mut framed[4..])
                .await
                .map_err(|e| e.to_string())?;
            let (frame, rest) = FrameKind::decode(&framed).map_err(|e| e.to_string())?;
            assert!(rest.is_empty(), "framing survived both hops");
            Ok(frame)
        })
        .await
        .map_err(|_| "frame timed out".to_owned())?
    }
}

async fn ping(t: &Topology, route: &str, token: Option<Vec<u8>>) -> Result<FrameKind, String> {
    let mut consumer = dial(t.addr, &t.fingerprint, route, token).await?;
    consumer
        .send(&FrameKind::Ping { nonce: PING_NONCE })
        .await?;
    consumer.recv().await
}

async fn wait_for_pong(t: &Topology) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_error = String::new();
    while Instant::now() < deadline {
        match ping(t, ROUTE, Some(CONSUMER_TOKEN.to_vec())).await {
            Ok(FrameKind::Pong { nonce }) if nonce == PING_NONCE => return,
            Ok(frame) => last_error = format!("unexpected frame: {frame:?}"),
            Err(err) => last_error = err,
        }
        sleep(Duration::from_millis(100)).await;
    }
    panic!("connector never served a consumer PING: {last_error}");
}

/// HELLO -> ATTACH -> snapshot -> live PTY echo, all through the relay.
#[test]
fn hello_attach_echo_through_relay_to_real_server() {
    let dir = TempDir::new().unwrap();
    provision(dir.path());
    phux_server_testkit::run_local(async {
        let t = Topology::start(dir, Some(CommandBuilder::new("/bin/cat")));
        wait_for_pong(&t).await;
        let mut consumer = dial(t.addr, &t.fingerprint, ROUTE, Some(CONSUMER_TOKEN.to_vec()))
            .await
            .unwrap();
        let hello = FrameKind::Hello {
            client_name: "relay-e2e-consumer".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::default(),
        };
        consumer.send(&hello).await.unwrap();
        consumer.send(&attach_by_name("default")).await.unwrap();

        let mut pane_id = None;
        let mut got_snapshot = false;
        while pane_id.is_none() || !got_snapshot {
            match consumer.recv().await.unwrap() {
                FrameKind::Attached { snapshot, .. } => {
                    assert_eq!(snapshot.resources.len(), 1, "exactly one pane");
                    pane_id = Some(snapshot.resources[0].id.clone());
                }
                FrameKind::BootstrapBegin { cols, rows, .. } => assert!(cols > 0 && rows > 0),
                FrameKind::BootstrapReady { .. } => got_snapshot = true,
                _ => {}
            }
        }
        let pane_id = pane_id.unwrap();

        let mut enter = ascii_key('\r', PhysicalKey::Enter);
        enter.text = None;
        enter.unshifted_codepoint = None;
        for event in [
            ascii_key('h', PhysicalKey::H),
            ascii_key('i', PhysicalKey::I),
            enter,
        ] {
            let input = FrameKind::InputKey {
                terminal_id: pane_id.clone(),
                event,
            };
            consumer.send(&input).await.unwrap();
        }
        let mut echoed = Vec::new();
        while !echoed.windows(2).any(|w| w == b"hi") {
            if let FrameKind::ResourceOutput { bytes, .. } = consumer.recv().await.unwrap() {
                echoed.extend_from_slice(&bytes);
            }
        }

        consumer.conn.close(0u32.into(), b"done");
        t.stop().await;
    });
}

/// The server still checks consumer bearers behind the relay, routes are
/// named by SNI, and the connector redials a restarted relay by itself.
#[test]
fn connector_bridges_consumers_rejects_bad_auth_and_redials() {
    let dir = TempDir::new().unwrap();
    provision(dir.path());
    phux_server_testkit::run_local(async {
        let mut t = Topology::start(dir, None);
        wait_for_pong(&t).await;

        let bad_auth = ping(&t, ROUTE, Some(vec![0x33; 32])).await;
        assert!(bad_auth.is_err(), "bad consumer token reached dispatch");
        wait_for_pong(&t).await;
        let wrong_route = ping(&t, "unknown-route", Some(CONSUMER_TOKEN.to_vec())).await;
        assert!(wrong_route.is_err(), "unknown route connected");

        t.restart_relay().await;
        wait_for_pong(&t).await;

        t.stop().await;
    });
}
