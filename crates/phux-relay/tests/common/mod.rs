//! Shared harness for the phux-relay integration tests.
//!
//! Everything under test is PRODUCTION code: [`spawn_relay`] runs the real
//! [`phux_relay::RelayRuntime`], consumers dial through the production
//! `phux_dial::quic::dial`, and the stub connector's tunnel leg goes
//! through the production `dial_with_alpn` with `QUIC_RELAY_ALPN`. The
//! only test-local logic is the connector's serving side (bearer check +
//! tagged echo backend), lifted from the spike's `spawn_connector` shape
//! (`crates/phux-server/tests/relay_connector_spike.rs`).
//!
//! Every await in a test body is bounded by a timeout: the
//! accept_bi-needs-bytes deadlock class must fail a test, never hang the
//! suite (nextest runs with `retries = 0`).

#![allow(dead_code, reason = "shared helpers; some binaries use a subset")]
#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::missing_panics_doc, reason = "tests")]
#![allow(unreachable_pub, reason = "tests/common shared-helpers pattern")]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use phux_dial::{CertTrust, DialError, QuicDial};
use phux_protocol::policy::QUIC_RELAY_ALPN;
use phux_relay::{AUTH_FAILED_CODE, RelayConfig, RelayError, RelayRuntime};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

/// Deadline applied to every wire recv. Mirrors the phux-server harness
/// value (and its rationale): generous so parallel-CI scheduler latency
/// never turns into a spurious failure; the happy path resolves in
/// milliseconds, so the ceiling only elapses on an actual fault.
pub const WIRE_RECV_TIMEOUT: Duration = Duration::from_secs(15);

/// Deadline for connection establishment / readiness polling, mirroring
/// the phux-server harness value with the same parallel-CI rationale.
pub const SOCKET_CONNECT_DEADLINE: Duration = Duration::from_secs(10);

/// The consumer-side bearer token. The relay never reads it (ADR-0051
/// Decision 4); the STUB connector — the server side of the tunnel —
/// verifies it before any consumer byte reaches the backend.
pub const CONSUMER_TOKEN: &[u8] = b"relay-test-consumer-0123456789ab";

/// A running production relay: its bound address, the pinnable certificate
/// fingerprint, and the token-store path tests mint routes into.
pub struct RelayHandle {
    pub addr: SocketAddr,
    pub fingerprint: String,
    pub tokens_path: PathBuf,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<Result<(), RelayError>>,
}

impl RelayHandle {
    /// Resolve the shutdown future and wait for the drain to finish.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = timeout(Duration::from_secs(5), self.task).await;
    }
}

/// An OS-assigned free loopback UDP address. The probe socket is dropped
/// before the relay re-binds the port; [`spawn_relay`] retries on the rare
/// race where another process grabs it in between.
fn free_udp_addr() -> SocketAddr {
    let probe = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind probe socket");
    probe.local_addr().expect("probe local addr")
}

/// Run the PRODUCTION `RelayRuntime::run_async` on the test runtime, with
/// all state files inside `dir`, and return its handle.
///
/// Certificate material is provisioned (via the production
/// `ensure_self_signed`) before startup so the fingerprint is pinnable and
/// the runtime's own provisioning is a no-op — which keeps the bind fast
/// enough for the port-race retry below to be reliable.
pub async fn spawn_relay(dir: &Path, max_conns: usize) -> RelayHandle {
    let cert_path = dir.join("relay-cert.pem");
    let key_path = dir.join("relay-key.pem");
    let tokens_path = dir.join("relay-tokens");
    phux_relay::ensure_self_signed(&cert_path, &key_path).expect("provision relay cert");
    let fingerprint = phux_relay::cert_fingerprint(&cert_path).expect("relay fingerprint");

    for _ in 0..5 {
        let addr = free_udp_addr();
        let config = RelayConfig {
            listen: addr,
            cert_path: cert_path.clone(),
            key_path: key_path.clone(),
            tokens_path: tokens_path.clone(),
            max_conns,
        };
        let (tx, rx) = oneshot::channel::<()>();
        let task = tokio::spawn(RelayRuntime::new(config).run_async(async move {
            let _ = rx.await;
        }));
        // The bind happens early in run_async (store load and TLS setup are
        // file reads); a lost port race surfaces as a prompt error return.
        sleep(Duration::from_millis(100)).await;
        if task.is_finished() {
            continue;
        }
        return RelayHandle {
            addr,
            fingerprint,
            tokens_path,
            shutdown: tx,
            task,
        };
    }
    panic!("could not bind a relay endpoint after 5 attempts");
}

/// Mint an enrollment token for `route` through the production library fn
/// and return the raw bytes a tunnel preamble carries.
pub fn mint(tokens_path: &Path, route: &str) -> Vec<u8> {
    let encoded = phux_relay::mint_route_token(tokens_path, route).expect("mint route token");
    phux_dial::quic::parse_token_hex(&encoded).expect("minted token is hex")
}

/// Dial the relay's tunnel leg through the production ALPN-parameterized
/// dialer: `QUIC_RELAY_ALPN`, pinned fingerprint, and (when `token` is
/// set) the stream-0 auth preamble already written.
pub async fn dial_tunnel_raw(
    relay_addr: SocketAddr,
    fingerprint: &str,
    server_name: &str,
    token: Option<Vec<u8>>,
) -> Result<
    (
        quinn::Endpoint,
        quinn::Connection,
        quinn::SendStream,
        quinn::RecvStream,
    ),
    DialError,
> {
    let dial = QuicDial {
        addr: relay_addr,
        server_name: server_name.to_owned(),
        token,
        trust: CertTrust::Pinned(fingerprint.to_owned()),
    };
    timeout(
        SOCKET_CONNECT_DEADLINE,
        phux_dial::quic::dial_with_alpn(&dial, QUIC_RELAY_ALPN),
    )
    .await
    .expect("tunnel dial resolves within deadline")
}

/// Everything the tests assert about a stub connector after the fact,
/// mirroring the spike's `ConnectorState` observability.
#[derive(Default)]
pub struct ConnectorState {
    pub bridged: usize,
    pub rejected_consumers: usize,
    pub bridged_stream_ids: Vec<quinn::StreamId>,
    /// Per admitted stream: every post-preamble byte the consumer sent,
    /// appended BEFORE echoing, so cross-talk is detectable at byte level.
    pub taps: Vec<Arc<Mutex<Vec<u8>>>>,
}

/// A live stub connector: its tunnel connection plus the shared state the
/// serving task appends to.
pub struct ConnectorHandle {
    pub state: Arc<Mutex<ConnectorState>>,
    pub conn: quinn::Connection,
    _endpoint: quinn::Endpoint,
    _task: JoinHandle<()>,
}

impl ConnectorHandle {
    /// Clean-close the tunnel connection (a connector going away).
    pub fn close(&self) {
        self.conn.close(0u32.into(), b"connector done");
    }

    /// Await the tunnel connection's close, bounded.
    pub async fn closed(&self) -> quinn::ConnectionError {
        timeout(WIRE_RECV_TIMEOUT, self.conn.closed())
            .await
            .expect("tunnel close resolves within deadline")
    }

    /// Consumers admitted (bearer verified) and served.
    pub fn bridged(&self) -> usize {
        self.state.lock().unwrap().bridged
    }

    /// Consumers refused at the bearer check (per-stream, tunnel kept).
    pub fn rejected(&self) -> usize {
        self.state.lock().unwrap().rejected_consumers
    }

    /// Relay-initiated streams that ever reached this connector.
    pub fn streams_seen(&self) -> usize {
        self.state.lock().unwrap().bridged_stream_ids.len()
    }

    /// Snapshot of every stream's consumer-byte tap.
    pub fn tapped_bytes(&self) -> Vec<Vec<u8>> {
        self.state
            .lock()
            .unwrap()
            .taps
            .iter()
            .map(|t| t.lock().unwrap().clone())
            .collect()
    }
}

/// Dial out to the relay as a stub connector (the spike's `spawn_connector`
/// shape, with the tunnel leg collapsed into the production
/// `dial_with_alpn`): register the tunnel with `tunnel_token`, hold the
/// reserved stream 0 open, then serve every relay-initiated bidi stream —
/// verify the consumer's bearer preamble (reset/stop with
/// `AUTH_FAILED_CODE` on mismatch), write `tag` once, and echo every
/// consumer byte back, tapping it first.
///
/// `server_name` is the SNI offered on the tunnel dial. It is NOT
/// load-bearing for connector admission (the token alone binds the route);
/// tests pass the route name by convention and one test pins the
/// distinction explicitly.
pub async fn spawn_connector(
    relay_addr: SocketAddr,
    fingerprint: &str,
    server_name: &str,
    tunnel_token: Vec<u8>,
    tag: &'static [u8],
) -> ConnectorHandle {
    let (endpoint, conn, send0, recv0) =
        dial_tunnel_raw(relay_addr, fingerprint, server_name, Some(tunnel_token))
            .await
            .expect("connector leg establishes (pin + relay ALPN + token preamble)");
    let state = Arc::new(Mutex::new(ConnectorState::default()));
    let task_state = Arc::clone(&state);
    let task_conn = conn.clone();
    let task = tokio::spawn(async move {
        // Stream 0 carries ONLY the auth preamble and is held open,
        // reserved: dropping the halves would FIN/STOP it.
        let _reserved_stream0 = (send0, recv0);
        while let Ok((mut tun_send, mut tun_recv)) = task_conn.accept_bi().await {
            task_state
                .lock()
                .unwrap()
                .bridged_stream_ids
                .push(tun_send.id());
            // The consumer's bearer crossed the relay opaquely; verify it
            // HERE — the server side of the tunnel — before any byte
            // reaches the backend (ADR-0051 Decision 4).
            let Some(bearer) = read_preamble(&mut tun_recv).await else {
                continue;
            };
            if bearer != CONSUMER_TOKEN {
                task_state.lock().unwrap().rejected_consumers += 1;
                let _ = tun_send.reset(AUTH_FAILED_CODE.into());
                let _ = tun_recv.stop(AUTH_FAILED_CODE.into());
                continue;
            }
            let stream_tap = Arc::new(Mutex::new(Vec::new()));
            {
                let mut s = task_state.lock().unwrap();
                s.bridged += 1;
                s.taps.push(Arc::clone(&stream_tap));
            }
            // Echo backend: the tag once at stream start, then every byte
            // back verbatim — tag first makes cross-talk visible, verbatim
            // echo keeps expected bytes deterministic under fragmentation.
            tokio::spawn(async move {
                if tun_send.write_all(tag).await.is_err() {
                    return;
                }
                let mut buf = [0u8; 4096];
                loop {
                    let Ok(Some(n)) = tun_recv.read(&mut buf).await else {
                        return;
                    };
                    stream_tap.lock().unwrap().extend_from_slice(&buf[..n]);
                    if tun_send.write_all(&buf[..n]).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    ConnectorHandle {
        state,
        conn,
        _endpoint: endpoint,
        _task: task,
    }
}

/// Read one length-prefixed auth preamble (`len: u32 BE` + raw bytes),
/// bounded like the production reader; `None` on short read, oversize, or
/// the [`WIRE_RECV_TIMEOUT`] deadline.
pub async fn read_preamble(recv: &mut quinn::RecvStream) -> Option<Vec<u8>> {
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

/// A consumer leg established through the PRODUCTION dialer.
pub struct Consumer {
    /// Keeps the I/O driver alive for the connection's lifetime.
    pub _endpoint: quinn::Endpoint,
    pub conn: quinn::Connection,
    pub send: quinn::SendStream,
    pub recv: quinn::RecvStream,
}

/// Dial the relay as a consumer via the production `phux_dial::quic::dial`:
/// production ALPN, `route` as the TLS SNI, pinned relay fingerprint, and
/// `bearer` (when set) written as the auth preamble.
pub async fn dial_consumer_with_bearer(
    relay_addr: SocketAddr,
    fingerprint: &str,
    route: &str,
    bearer: Option<Vec<u8>>,
) -> Result<Consumer, DialError> {
    let dial = QuicDial {
        addr: relay_addr,
        server_name: route.to_owned(),
        token: bearer,
        trust: CertTrust::Pinned(fingerprint.to_owned()),
    };
    timeout(SOCKET_CONNECT_DEADLINE, phux_dial::quic::dial(&dial))
        .await
        .expect("consumer dial resolves within deadline")
        .map(|(endpoint, conn, send, recv)| Consumer {
            _endpoint: endpoint,
            conn,
            send,
            recv,
        })
}

/// [`dial_consumer_with_bearer`] with the well-known [`CONSUMER_TOKEN`].
pub async fn dial_consumer(
    relay_addr: SocketAddr,
    fingerprint: &str,
    route: &str,
) -> Result<Consumer, DialError> {
    dial_consumer_with_bearer(
        relay_addr,
        fingerprint,
        route,
        Some(CONSUMER_TOKEN.to_vec()),
    )
    .await
}

/// Send `payload` and read back exactly `tag + payload` (the echo
/// connector writes its tag once at stream start — pass an empty tag for
/// follow-up echoes on the same stream).
pub async fn expect_echo(consumer: &mut Consumer, tag: &[u8], payload: &[u8]) {
    consumer
        .send
        .write_all(payload)
        .await
        .expect("send payload");
    let mut expected = tag.to_vec();
    expected.extend_from_slice(payload);
    let mut got = vec![0u8; expected.len()];
    timeout(WIRE_RECV_TIMEOUT, consumer.recv.read_exact(&mut got))
        .await
        .expect("echo within deadline")
        .expect("echo read");
    assert_eq!(got, expected, "echo must be tag + payload, byte-identical");
}

/// Wait until `route` has a live tunnel at the relay, WITHOUT touching the
/// connector: dial as a consumer but send no bearer preamble. A route with
/// no tunnel is application-closed (`ROUTE_OFFLINE`) promptly; a live one
/// leaves the probe connection open, because the relay's consumer-side
/// `accept_bi` only resolves on the consumer's first bytes — which the
/// probe never sends — so no bridge stream ever reaches the connector and
/// no connector-side counter or tap moves.
pub async fn await_route_live(relay_addr: SocketAddr, fingerprint: &str, route: &str) {
    let deadline = tokio::time::Instant::now() + SOCKET_CONNECT_DEADLINE;
    while tokio::time::Instant::now() < deadline {
        let probe = dial_consumer_with_bearer(relay_addr, fingerprint, route, None).await;
        if let Ok(consumer) = probe {
            // An elapsed window means the connection is still open — the
            // route is live (idle timeout is 30s, so only an admitted
            // connection survives the window). A close under the window
            // means no live tunnel yet; retry.
            if timeout(Duration::from_millis(150), consumer.conn.closed())
                .await
                .is_err()
            {
                consumer.conn.close(0u32.into(), b"probe done");
                return;
            }
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("route {route} never came live at the relay");
}

/// Dial + echo with retries until the route serves, for windows where the
/// registry is settling (a redial replacing a dead tunnel). Retried
/// attempts fail BEFORE bridging (`ROUTE_OFFLINE` or a dead-tunnel
/// `open_bi`), so connector-side counters and taps only record the one
/// successful bridge.
pub async fn echo_when_ready(
    relay_addr: SocketAddr,
    fingerprint: &str,
    route: &str,
    tag: &[u8],
    payload: &[u8],
) -> Consumer {
    let deadline = tokio::time::Instant::now() + SOCKET_CONNECT_DEADLINE;
    while tokio::time::Instant::now() < deadline {
        if let Ok(mut consumer) = dial_consumer(relay_addr, fingerprint, route).await {
            if consumer.send.write_all(payload).await.is_ok() {
                let mut expected = tag.to_vec();
                expected.extend_from_slice(payload);
                let mut got = vec![0u8; expected.len()];
                match timeout(WIRE_RECV_TIMEOUT, consumer.recv.read_exact(&mut got)).await {
                    Ok(Ok(())) => {
                        assert_eq!(got, expected, "echo must be tag + payload");
                        return consumer;
                    }
                    // Stream ended before the echo: refused pre-bridge; retry.
                    Ok(Err(_)) => {}
                    Err(elapsed) => {
                        panic!("echo timed out on what should be a live bridge: {elapsed}")
                    }
                }
            }
            consumer.conn.close(0u32.into(), b"retry");
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("route {route} never served an echo");
}

/// Assert `err` is an application close carrying `code`.
pub fn assert_app_closed(err: &quinn::ConnectionError, code: u32, what: &str) {
    match err {
        quinn::ConnectionError::ApplicationClosed(app) => {
            assert_eq!(
                app.error_code,
                quinn::VarInt::from(code),
                "{what}: wrong close code (reason {:?})",
                String::from_utf8_lossy(&app.reason)
            );
        }
        other => panic!("{what}: expected an application close, got {other:?}"),
    }
}

/// Assert the consumer-visible outcome of a refusal that happens AFTER a
/// completed TLS handshake: either the dial returned a connection that is
/// then application-closed with `code`, or (when the close won the race
/// against `open_bi`/preamble) the dial error already carries the close's
/// `reason` text — either way the refusal is post-handshake and
/// distinguishable from a TLS-layer refusal.
pub async fn expect_post_handshake_close(
    result: Result<Consumer, DialError>,
    code: u32,
    reason: &str,
) {
    match result {
        Ok(consumer) => {
            let err = timeout(WIRE_RECV_TIMEOUT, consumer.conn.closed())
                .await
                .expect("close resolves within deadline");
            assert_app_closed(&err, code, reason);
        }
        Err(err) => {
            let msg = err.to_string();
            assert!(
                msg.contains(reason),
                "dial error should carry the app-close reason {reason:?}, got: {msg}"
            );
        }
    }
}

/// Whether `haystack` contains `needle` as a contiguous subslice.
pub fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
