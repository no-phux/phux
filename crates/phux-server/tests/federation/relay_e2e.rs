//! Dial-out relay end to end (ADR-0051/0052/0053): a server whose only
//! listener is a UDS in a tempdir dials OUT to the production `RelayRuntime`
//! through its production connector, and consumers reach it through the relay
//! with the production QUIC dialer.
//!
//! These cases are about routing, so the server runs an embedder engine
//! (ADR-0072) and the consumer presents no certificate; it still runs its
//! session end to end with the server inside the relayed stream, the only
//! shape a connector admits (ADR-0180). `relay_inner_tls` covers `paired`
//! and `relay_blind` what the relay can see.
//!
//! ```text
//! consumer --QUIC "phux-quic/1"--> relay <--"phux-relay/1"-- connector (in server) --> dispatch
//!          \______________ TLS 1.3 (server CA pin) ______________/
//! ```

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use phux_dial::TlsClientIdentity;
use phux_dial::quic::DialedStream;
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::frame::FrameKind;
use phux_server::LaunchCommand as CommandBuilder;
use phux_server_testkit::relay::RelayHarness;
use phux_server_testkit::{WIRE_RECV_TIMEOUT, ascii_key, attach_by_name};
use tempfile::TempDir;
use tokio::time::{sleep, timeout};

use crate::relay_common::{
    RelayedServer, embedder_seeded, hello, inner, loopback, plan, read_frame, relay_config,
    relay_pin, write_frame,
};

const ROUTE: &str = "connector-test";
const PING_NONCE: u64 = 0xC011_EC70;
const TUNNEL_TOKEN: [u8; 32] = [0x11; 32];

/// A relay and a connector-configured server behind it, sharing a tempdir.
struct Topology {
    dir: TempDir,
    relay: Option<RelayHarness>,
    addr: SocketAddr,
    fingerprint: String,
    server: RelayedServer,
}

impl Topology {
    /// Start the relay, then a server (optionally seeding session `default`
    /// with a PTY running `seed`) whose connector dials it.
    fn start(seed: Option<CommandBuilder>) -> Self {
        let dir = TempDir::new().unwrap();
        let relay = RelayHarness::start(relay_config(dir.path(), loopback(), ROUTE, &TUNNEL_TOKEN));
        let fingerprint = relay_pin(dir.path());
        let server = RelayedServer::start(
            dir.path(),
            relay.addr,
            &fingerprint,
            &TUNNEL_TOKEN,
            |cfg, _| embedder_seeded(cfg, seed),
        );
        Self {
            dir,
            addr: relay.addr,
            relay: Some(relay),
            fingerprint,
            server,
        }
    }

    async fn stop(self) {
        self.server.stop().await;
        self.relay.expect("relay running").stop().await;
    }

    /// Stop the relay and start a fresh one on the same address.
    async fn restart_relay(&mut self) {
        self.relay.take().expect("relay running").stop().await;
        let config = relay_config(self.dir.path(), self.addr, ROUTE, &TUNNEL_TOKEN);
        let restarted = RelayHarness::start(config);
        assert_eq!(restarted.addr, self.addr);
        self.relay = Some(restarted);
    }
}

/// A consumer's end-to-end session through the relay.
struct Consumer {
    _endpoint: quinn::Endpoint,
    conn: quinn::Connection,
    send: tokio::io::WriteHalf<phux_dial::quic::InnerStream>,
    recv: tokio::io::ReadHalf<phux_dial::quic::InnerStream>,
}

async fn dial(t: &Topology, route: &str, token: Option<Vec<u8>>) -> Result<Consumer, String> {
    let session = inner(&t.server.authority, TlsClientIdentity::None, route);
    let dial = plan(t.addr, &t.fingerprint, route, token, Some(session));
    let (endpoint, conn, stream) = phux_dial::quic::dial_stream(&dial)
        .await
        .map_err(|err| err.to_string())?;
    let DialedStream::Inner(stream) = stream else {
        return Err("a relayed dial runs an end-to-end session".to_owned());
    };
    let (recv, send) = tokio::io::split(*stream);
    Ok(Consumer {
        _endpoint: endpoint,
        conn,
        send,
        recv,
    })
}

impl Consumer {
    async fn send(&mut self, frame: &FrameKind) -> Result<(), String> {
        write_frame(&mut self.send, frame)
            .await
            .map_err(|err| err.to_string())
    }

    async fn recv(&mut self) -> Result<FrameKind, String> {
        timeout(WIRE_RECV_TIMEOUT, read_frame(&mut self.recv))
            .await
            .map_err(|_| "frame timed out".to_owned())?
            .map_err(|err| err.to_string())
    }
}

async fn ping(t: &Topology, route: &str, token: Option<Vec<u8>>) -> Result<FrameKind, String> {
    let mut consumer = dial(t, route, token).await?;
    consumer
        .send(&FrameKind::Ping { nonce: PING_NONCE })
        .await?;
    consumer.recv().await
}

async fn wait_for_pong(t: &Topology) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_error = String::new();
    while Instant::now() < deadline {
        match ping(t, ROUTE, Some(t.server.bearer.clone())).await {
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
    phux_server_testkit::run_local(async {
        let t = Topology::start(Some(CommandBuilder::new("/bin/cat")));
        wait_for_pong(&t).await;
        let mut consumer = dial(&t, ROUTE, Some(t.server.bearer.clone()))
            .await
            .unwrap();
        consumer.send(&hello("relay-e2e-consumer")).await.unwrap();
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
            let input = phux_server_testkit::input_key(pane_id.clone(), event);
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
    phux_server_testkit::run_local(async {
        let mut t = Topology::start(None);
        wait_for_pong(&t).await;

        let bad_auth = ping(&t, ROUTE, Some(vec![0x33; 32])).await;
        assert!(bad_auth.is_err(), "bad consumer token reached dispatch");
        wait_for_pong(&t).await;
        let wrong_route = ping(&t, "unknown-route", Some(t.server.bearer.clone())).await;
        assert!(wrong_route.is_err(), "unknown route connected");

        t.restart_relay().await;
        wait_for_pong(&t).await;

        t.stop().await;
    });
}
