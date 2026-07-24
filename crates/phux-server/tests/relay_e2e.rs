//! Full-stack relay end-to-end (ADR-0051/0052/0053): a real `phux-server`
//! behind a NAT-shaped UDS is reached through the PRODUCTION
//! `phux_relay::RelayRuntime` by a consumer using the PRODUCTION
//! `phux_dial::quic::dial` — full HELLO -> ATTACH -> ATTACHED +
//! `TERMINAL_SNAPSHOT` handshake and a live PTY echo.
//!
//! Topology (the spike's, with the stub relay swapped for the real one):
//!
//! ```text
//! consumer --QUIC "phux-quic/1"--> RelayRuntime <--"phux-relay/1"-- connector --UDS--> server
//! ```
//!
//! The connector is the only remaining stub (production connector is a
//! later bead): it authenticates with a minted route token via the
//! production `dial_with_alpn`, verifies each bridged consumer's bearer
//! (the server side of the tunnel, ADR-0051 Decision 4), and splices onto
//! a fresh UDS connection — lifted from the spike's `spawn_connector`.
//!
//! Living in `crates/phux-server/tests/` puts this binary in the
//! `pty-serial` nextest group automatically (it drives a real PTY).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

mod common;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use phux_dial::{CertTrust, QuicDial};
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::policy::QUIC_RELAY_ALPN;
use phux_protocol::wire::frame::FrameKind;
use phux_relay::{AUTH_FAILED_CODE, RelayConfig, RelayRuntime};
use tempfile::TempDir;
use tokio::time::{sleep, timeout};

use crate::common::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, encode_frame, run_local,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// The consumer bearer the STUB connector verifies and strips before
/// splicing to the server's UDS; the relay passes it opaquely.
const CONSUMER_TOKEN: &[u8] = b"relay-e2e-consumer-0123456789abc";

/// A running production relay endpoint for this test.
struct RelayHandle {
    addr: SocketAddr,
    fingerprint: String,
    tokens_path: PathBuf,
}

/// Run the PRODUCTION `RelayRuntime` on the test runtime, retrying the
/// OS-assigned-port race (mirrors the phux-relay integration harness).
async fn spawn_relay(dir: &Path) -> RelayHandle {
    let cert_path = dir.join("relay-cert.pem");
    let key_path = dir.join("relay-key.pem");
    let tokens_path = dir.join("relay-tokens");
    phux_relay::ensure_self_signed(&cert_path, &key_path).expect("provision relay cert");
    let fingerprint = phux_relay::cert_fingerprint(&cert_path).expect("relay fingerprint");
    for _ in 0..5 {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").expect("probe socket");
        let addr = probe.local_addr().expect("probe addr");
        drop(probe);
        let config = RelayConfig {
            listen: addr,
            cert_path: cert_path.clone(),
            key_path: key_path.clone(),
            tokens_path: tokens_path.clone(),
            max_conns: phux_relay::DEFAULT_MAX_CONNS,
        };
        let task = tokio::spawn(RelayRuntime::new(config).run_async(std::future::pending()));
        sleep(Duration::from_millis(100)).await;
        if task.is_finished() {
            continue;
        }
        return RelayHandle {
            addr,
            fingerprint,
            tokens_path,
        };
    }
    panic!("could not bind a relay endpoint after 5 attempts");
}

/// Read one auth preamble off a quinn recv stream, bounded.
async fn read_preamble(recv: &mut quinn::RecvStream) -> Option<Vec<u8>> {
    timeout(WIRE_RECV_TIMEOUT, async {
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf).await.ok()?;
        let len = usize::try_from(u32::from_be_bytes(len_buf)).ok()?;
        if len > 256 {
            return None;
        }
        let mut token = vec![0u8; len];
        recv.read_exact(&mut token).await.ok()?;
        Some(token)
    })
    .await
    .ok()
    .flatten()
}

/// The stub connector (spike `spawn_connector`, tunnel leg collapsed into
/// the production `dial_with_alpn`): register the tunnel with the minted
/// route token, hold stream 0 open, then per bridged stream verify the
/// consumer bearer and splice onto a FRESH UDS connection to the server.
async fn spawn_uds_connector(
    relay_addr: SocketAddr,
    fingerprint: &str,
    route: &str,
    tunnel_token: Vec<u8>,
    socket_path: PathBuf,
) -> (quinn::Endpoint, quinn::Connection) {
    let dial = QuicDial {
        addr: relay_addr,
        server_name: route.to_owned(),
        token: Some(tunnel_token),
        trust: CertTrust::Pinned(fingerprint.to_owned()),
    };
    let (endpoint, conn, send0, recv0) = timeout(
        SOCKET_CONNECT_DEADLINE,
        phux_dial::quic::dial_with_alpn(&dial, QUIC_RELAY_ALPN),
    )
    .await
    .expect("connector dial within deadline")
    .expect("connector leg establishes (pin + relay ALPN + token preamble)");
    let task_conn = conn.clone();
    tokio::spawn(async move {
        // Stream 0 carries ONLY the auth preamble and is held open.
        let _reserved_stream0 = (send0, recv0);
        while let Ok((mut tun_send, mut tun_recv)) = task_conn.accept_bi().await {
            // The consumer's bearer crossed the relay opaquely; verify it
            // HERE, before any byte reaches the server (Decision 4).
            let Some(bearer) = read_preamble(&mut tun_recv).await else {
                continue;
            };
            if bearer != CONSUMER_TOKEN {
                let _ = tun_send.reset(AUTH_FAILED_CODE.into());
                let _ = tun_recv.stop(AUTH_FAILED_CODE.into());
                continue;
            }
            // Fresh UDS connection per admitted consumer, spliced with the
            // stdio-bridge shape: one finished direction ends the bridge.
            let socket_path = socket_path.clone();
            tokio::spawn(async move {
                let uds = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
                let (mut from_server, mut to_server) = uds.into_split();
                tokio::select! {
                    _ = tokio::io::copy(&mut tun_recv, &mut to_server) => {}
                    _ = tokio::io::copy(&mut from_server, &mut tun_send) => {}
                }
            });
        }
    });
    (endpoint, conn)
}

/// Wait until `route` has a live tunnel, without touching the connector:
/// a bearer-less probe is closed promptly when the route is offline and
/// left open (consumer `accept_bi` needs bytes it never sends) when live.
async fn await_route_live(relay_addr: SocketAddr, fingerprint: &str, route: &str) {
    let deadline = tokio::time::Instant::now() + SOCKET_CONNECT_DEADLINE;
    while tokio::time::Instant::now() < deadline {
        let dial = QuicDial {
            addr: relay_addr,
            server_name: route.to_owned(),
            token: None,
            trust: CertTrust::Pinned(fingerprint.to_owned()),
        };
        if let Ok(Ok((_ep, conn, _send, _recv))) =
            timeout(SOCKET_CONNECT_DEADLINE, phux_dial::quic::dial(&dial)).await
        {
            // Elapsed window: still open, so the route is live. A close
            // under the window means no tunnel yet; retry.
            if timeout(Duration::from_millis(150), conn.closed())
                .await
                .is_err()
            {
                conn.close(0u32.into(), b"probe done");
                return;
            }
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("route {route} never came live at the relay");
}

/// A consumer leg through the production dialer, with helpers for the
/// framed phux wire.
struct Consumer {
    /// Keeps the I/O driver alive for the connection's lifetime.
    _endpoint: quinn::Endpoint,
    conn: quinn::Connection,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
}

async fn dial_consumer(relay_addr: SocketAddr, fingerprint: &str, route: &str) -> Consumer {
    let dial = QuicDial {
        addr: relay_addr,
        server_name: route.to_owned(),
        token: Some(CONSUMER_TOKEN.to_vec()),
        trust: CertTrust::Pinned(fingerprint.to_owned()),
    };
    let (endpoint, conn, send, recv) =
        timeout(SOCKET_CONNECT_DEADLINE, phux_dial::quic::dial(&dial))
            .await
            .expect("consumer dial within deadline")
            .expect("consumer dials the relay through the production dialer");
    Consumer {
        _endpoint: endpoint,
        conn,
        send,
        recv,
    }
}

async fn send_wire(consumer: &mut Consumer, frame: &FrameKind) {
    let buf = encode_frame(frame);
    consumer.send.write_all(&buf).await.expect("send frame");
}

/// Read + decode one frame; the full-consumption check proves the 4-byte
/// length-prefix framing survived two QUIC hops and the UDS splice intact.
async fn recv_wire(consumer: &mut Consumer) -> FrameKind {
    let framed = timeout(WIRE_RECV_TIMEOUT, async {
        let mut header = [0u8; 4];
        consumer.recv.read_exact(&mut header).await.expect("header");
        let len = u32::from_be_bytes(header) as usize;
        let mut framed = header.to_vec();
        framed.resize(4 + len, 0);
        consumer
            .recv
            .read_exact(&mut framed[4..])
            .await
            .expect("body");
        framed
    })
    .await
    .expect("timed out waiting for frame");
    let (frame, rest) = FrameKind::decode(&framed).expect("decode frame through the relay");
    assert!(
        rest.is_empty(),
        "decoder must consume the entire frame: framing survived both hops"
    );
    frame
}

fn hello(client_name: &str) -> FrameKind {
    FrameKind::Hello {
        client_name: client_name.to_owned(),
        protocol_major: 0,
        protocol_minor: 2,
        protocol_patch: 0,
        client_caps: ClientCapabilities::default(),
    }
}

/// An ASCII printable key press (crib of `input_dispatch.rs`).
fn ascii_key(c: char, key: PhysicalKey) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: Some(c.to_string()),
        unshifted_codepoint: Some(c as u32),
    }
}

/// An Enter key — no `text`, libghostty's encoder synthesizes the CR.
const fn enter_key() -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::Enter,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

/// Drain `TERMINAL_OUTPUT` frames until `needle` appears in the
/// accumulated VT bytes or the deadline elapses.
async fn await_echo(consumer: &mut Consumer, needle: &[u8]) {
    let mut acc: Vec<u8> = Vec::new();
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if let FrameKind::TerminalOutput { bytes, .. } = recv_wire(consumer).await {
            acc.extend_from_slice(&bytes);
            if acc.windows(needle.len()).any(|w| w == needle) {
                return;
            }
        }
    }
    panic!(
        "input must round-trip through relay + tunnel to the PTY and echo back \
         as TERMINAL_OUTPUT (got {} bytes: {acc:?})",
        acc.len()
    );
}

/// Requirement 7: the full production wire through the production relay —
/// HELLO -> ATTACH `ByName` 80x24 -> ATTACHED + `TERMINAL_SNAPSHOT` decode
/// as typed frames (relay byte-transparency: no reframing, no ack
/// emission — ADR-0051 invariants 1 and 5), then a live PTY echo from
/// `/bin/cat`.
#[test]
#[allow(clippy::too_many_lines, reason = "one linear protocol scenario")]
fn hello_attach_echo_through_relay_to_real_server() {
    run_local(async {
        // NAT invariant: the server's ONLY listener is this UDS in a
        // tempdir; the only inbound network socket below is the relay's.
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let cmd = portable_pty::CommandBuilder::new("/bin/cat");
        let (shutdown, server) = spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        // The PRODUCTION relay, with a route minted through the
        // production enrollment fn.
        let relay = spawn_relay(tmp.path()).await;
        let token_hex =
            phux_relay::mint_route_token(&relay.tokens_path, "alpha").expect("mint route token");
        let token = phux_dial::quic::parse_token_hex(&token_hex).expect("token hex");

        let (_conn_ep, _conn) = spawn_uds_connector(
            relay.addr,
            &relay.fingerprint,
            "alpha",
            token,
            socket_path.clone(),
        )
        .await;
        await_route_live(relay.addr, &relay.fingerprint, "alpha").await;

        // The production consumer wire, end to end.
        let mut consumer = dial_consumer(relay.addr, &relay.fingerprint, "alpha").await;
        send_wire(&mut consumer, &hello("relay-e2e-consumer")).await;
        send_wire(&mut consumer, &attach_by_name("default")).await;

        let mut pane_id = None;
        let mut got_snapshot = false;
        while pane_id.is_none() || !got_snapshot {
            match recv_wire(&mut consumer).await {
                FrameKind::Attached { snapshot, .. } => {
                    assert_eq!(snapshot.panes.len(), 1, "exactly one pane");
                    pane_id = Some(snapshot.panes[0].id.clone());
                }
                FrameKind::TerminalSnapshot { cols, rows, .. } => {
                    assert!(cols > 0 && rows > 0, "snapshot has a real grid");
                    got_snapshot = true;
                }
                _ => {}
            }
        }
        let pane_id = pane_id.unwrap();

        // Live PTY echo: type "hi" + Enter into the `cat` pane; the echo
        // must come back as TERMINAL_OUTPUT frames through both hops.
        for (c, key) in [('h', PhysicalKey::H), ('i', PhysicalKey::I)] {
            send_wire(
                &mut consumer,
                &FrameKind::InputKey {
                    terminal_id: pane_id.clone(),
                    event: ascii_key(c, key),
                },
            )
            .await;
        }
        send_wire(
            &mut consumer,
            &FrameKind::InputKey {
                terminal_id: pane_id,
                event: enter_key(),
            },
        )
        .await;
        await_echo(&mut consumer, b"hi").await;

        // Clean teardown: consumer close, then server shutdown. The relay
        // and connector tasks drop with the runtime.
        consumer.conn.close(0u32.into(), b"done");
        shutdown.send(()).ok();
        timeout(Duration::from_secs(5), server)
            .await
            .expect("server didn't shut down in time")
            .expect("server join")
            .expect("server run_async ok");
    });
}
