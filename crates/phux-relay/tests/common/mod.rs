//! Shared harness for the phux-relay integration tests.
//!
//! Everything under test is PRODUCTION code: the real relay runtime and the
//! production dialers for both legs. The only test-local logic is the stub
//! connector's serving side (bearer check + tagged echo backend). Every await
//! is bounded so a deadlock fails a test instead of hanging the suite.

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

/// Deadline applied to every wire recv; generous for loaded CI.
pub const WIRE_RECV_TIMEOUT: Duration = Duration::from_secs(15);

/// Deadline for connection establishment and readiness polling.
pub const SOCKET_CONNECT_DEADLINE: Duration = Duration::from_secs(10);

/// The consumer-side bearer token: opaque to the relay, verified by the stub
/// connector (ADR-0051 Decision 4).
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

/// Run the PRODUCTION relay (`bind` on port 0, then `serve`) on the test
/// runtime with all state files inside `dir`, pre-provisioning the
/// certificate so its fingerprint is pinnable. `preamble_deadline`
/// overrides the tunnel auth-preamble deadline when set.
pub async fn spawn_relay_with(
    dir: &Path,
    max_conns: usize,
    preamble_deadline: Option<Duration>,
) -> RelayHandle {
    spawn_relay_tuned(dir, max_conns, |runtime| match preamble_deadline {
        Some(deadline) => runtime.with_preamble_deadline(deadline),
        None => runtime,
    })
    .await
}

/// [`spawn_relay_with`], with `tune` applied to the runtime before it binds.
pub async fn spawn_relay_tuned(
    dir: &Path,
    max_conns: usize,
    tune: impl FnOnce(RelayRuntime) -> RelayRuntime,
) -> RelayHandle {
    let cert_path = dir.join("relay-cert.pem");
    let key_path = dir.join("relay-key.pem");
    let tokens_path = dir.join("relay-tokens");
    phux_relay::ensure_self_signed(&cert_path, &key_path).expect("provision relay cert");
    let fingerprint = phux_relay::cert_fingerprint(&cert_path).expect("relay fingerprint");
    let runtime = tune(RelayRuntime::new(RelayConfig {
        listen: "127.0.0.1:0".parse().expect("loopback listen addr"),
        cert_path,
        key_path,
        tokens_path: tokens_path.clone(),
        max_conns,
    }));
    let bound = runtime.bind().expect("relay binds on port 0");
    let addr = bound.local_addr();
    let (shutdown, rx) = oneshot::channel::<()>();
    let task = tokio::spawn(bound.serve(async move {
        let _ = rx.await;
    }));
    RelayHandle {
        addr,
        fingerprint,
        tokens_path,
        shutdown,
        task,
    }
}

/// [`spawn_relay_with`] and the production preamble deadline.
pub async fn spawn_relay(dir: &Path, max_conns: usize) -> RelayHandle {
    spawn_relay_with(dir, max_conns, None).await
}

/// Mint an enrollment token for `route` through the production library fn
/// and return the raw bytes a tunnel preamble carries.
pub fn mint(tokens_path: &Path, route: &str) -> Vec<u8> {
    let encoded = phux_relay::mint_route_token(tokens_path, route).expect("mint route token");
    phux_dial::quic::parse_token_hex(&encoded).expect("minted token is hex")
}

/// Dial the relay's tunnel leg with `QUIC_RELAY_ALPN`, the pinned
/// fingerprint, and (when `token` is set) the stream-0 auth preamble.
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
        identity: None,
    };
    timeout(
        SOCKET_CONNECT_DEADLINE,
        phux_dial::quic::dial_with_alpn(&dial, QUIC_RELAY_ALPN),
    )
    .await
    .expect("tunnel dial resolves within deadline")
}

/// What the stub connector observed, for after-the-fact assertions.
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

/// Dial out as a stub connector: register the tunnel with `tunnel_token`,
/// hold stream 0 open, then serve every relay-initiated stream by verifying
/// the consumer's bearer (reset with `AUTH_FAILED_CODE` on mismatch),
/// writing `tag` once, and echoing every byte back after tapping it.
/// `server_name` is the tunnel's SNI, which does not decide the route.
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
        // Stream 0 is reserved: dropping the halves would FIN/STOP it.
        let _reserved_stream0 = (send0, recv0);
        while let Ok((mut tun_send, mut tun_recv)) = task_conn.accept_bi().await {
            task_state
                .lock()
                .unwrap()
                .bridged_stream_ids
                .push(tun_send.id());
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
            // Tag first makes cross-talk visible; then verbatim echo.
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

/// Read one length-prefixed auth preamble; `None` on short read, oversize,
/// or timeout.
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

/// Dial the relay as a consumer via `phux_dial::quic::dial` with `route`
/// as SNI and `bearer` (when set) as the auth preamble.
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
        identity: None,
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

/// Send `payload` and read back `tag + payload` (the connector writes its
/// tag once per stream; pass an empty tag for follow-ups). `None` when the
/// stream ended first; a timeout or wrong bytes panic.
async fn try_echo(consumer: &mut Consumer, tag: &[u8], payload: &[u8]) -> Option<()> {
    consumer.send.write_all(payload).await.ok()?;
    let mut expected = tag.to_vec();
    expected.extend_from_slice(payload);
    let mut got = vec![0u8; expected.len()];
    timeout(WIRE_RECV_TIMEOUT, consumer.recv.read_exact(&mut got))
        .await
        .expect("echo within deadline")
        .ok()?;
    assert_eq!(got, expected, "echo must be tag + payload, byte-identical");
    Some(())
}

/// [`try_echo`] that must succeed.
pub async fn expect_echo(consumer: &mut Consumer, tag: &[u8], payload: &[u8]) {
    try_echo(consumer, tag, payload).await.expect("echo read");
}

/// Wait until `route` has a live tunnel without touching the connector: a
/// probe consumer that sends no bytes is closed `ROUTE_OFFLINE` promptly
/// when there is no tunnel, and left open (never bridged) when there is.
pub async fn await_route_live(relay_addr: SocketAddr, fingerprint: &str, route: &str) {
    let deadline = tokio::time::Instant::now() + SOCKET_CONNECT_DEADLINE;
    while tokio::time::Instant::now() < deadline {
        let probe = dial_consumer_with_bearer(relay_addr, fingerprint, route, None).await;
        // Still open after the window (idle timeout is 30s): the route is live.
        if let Ok(consumer) = probe
            && timeout(Duration::from_millis(150), consumer.conn.closed())
                .await
                .is_err()
        {
            consumer.conn.close(0u32.into(), b"probe done");
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("route {route} never came live at the relay");
}

/// Dial + echo with retries while the registry settles. Retried attempts
/// fail before bridging, so connector counters record only the success.
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
            if try_echo(&mut consumer, tag, payload).await.is_some() {
                return consumer;
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

/// Assert a post-handshake refusal: the connection is application-closed
/// with `code`, or (when the close won the race) the dial error carries
/// `reason`.
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
