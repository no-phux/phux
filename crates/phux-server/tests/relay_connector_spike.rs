//! Dial-out connector spike (ADR-0051, bead phux-1cfx): a server behind NAT
//! reaches remote consumers through a self-hosted relay it dials OUT to.
//!
//! **NAT invariant (structural, not simulated).** The only network socket in
//! this test that accepts inbound connections is the relay's. The server's
//! sole inbound doorway is a filesystem UDS inside a tempdir — reachable only
//! from "inside its NAT" — and the only network connection the server side
//! participates in is one the connector dialed OUT. The rendezvous ordering
//! is asserted: the tunnel registers at the relay while zero consumers exist.
//!
//! Topology under test (ADR-0051 Decisions 1-2):
//!
//! ```text
//! consumer --QUIC "phux-quic/1"--> relay <--QUIC "phux-relay/1"-- connector --UDS--> server
//! ```
//!
//! * The connector leg negotiates the dedicated ALPN literal `phux-relay/1`
//!   (never the production consumer ALPN — invariant 7) with a pinned cert
//!   fingerprint and a tunnel-token preamble, fail-closed both ways.
//! * The connector-initiated stream 0 carries ONLY the connector's auth
//!   preamble and is held open, reserved for future control use.
//! * Every admitted consumer is bridged as exactly one RELAY-initiated bidi
//!   stream, which the connector splices onto a FRESH UDS connection.
//! * The relay is byte-opaque past its own handshakes: its whole data path
//!   is per-stream byte pumps ([`pump_tapped`]) — it never decodes a
//!   `FrameKind` and never emits an ack (invariants 1 and 5). Opacity is
//!   by construction and additionally proven by byte-identity taps.
//! * Consumer bearer tokens pass through the relay opaquely and are verified
//!   on the server side of the tunnel; relay admission is never
//!   authorization (Decision 4).
//!
//! Spike tolerances (ADR-0051 Decision 7 — flagged, not silently assumed):
//!
//! * The "server side of the tunnel" verifying each consumer's token is the
//!   in-test connector task (the server's UDS listener carries no
//!   `TokenStore`); production routes bridged streams through the QUIC
//!   accept path and the server's own `TokenStore`. The fail-closed order is
//!   identical: the token is checked before any consumer byte reaches the
//!   server.
//! * Bridged consumers surface to the server with the connector's local UDS
//!   uid — acceptable in the spike, unacceptable in production.
//!
//! Explicitly OUT of spike scope (ADR-0051 assertion 7; these belong to the
//! implementation bead, and nothing here should be read as covering them):
//!
//! * redial/supervision — production reuses `hub/link.rs` (backoff,
//!   fail-closed `plan_link`-style gating, token file re-read per redial);
//! * per-consumer `PeerIdentity` — production bridged consumers are ordinary
//!   remote consumers (`transport: Quic`, `source_addr` = the relay);
//! * route identity — how a consumer names WHICH tunneled server.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(
    clippy::future_not_send,
    reason = "current-thread LocalSet runtime (ADR-0003/0014); Rc/RefCell shared state is deliberate"
)]

use std::cell::RefCell;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use phux_dial::{CertTrust, QuicDial};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::policy::QUIC_ALPN;
use phux_protocol::wire::frame::FrameKind;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, encode_frame, run_local,
    spawn_server, spawn_server_with_seed_cmd, wait_for_socket,
};

/// The connector leg's dedicated ALPN (ADR-0051 Decision 2). Reserved
/// normatively by the ADR; the `phux_protocol::policy` constant lands with
/// the implementation bead, so the spike spells it as a literal — and it
/// must NEVER be the production consumer ALPN (invariant 7).
const RELAY_ALPN: &[u8] = b"phux-relay/1";

/// The connector's tunnel-registration token — the relay-enrollment secret
/// (ADR-0051 Decision 4, connector-to-relay leg). 32 bytes, mirroring
/// `auth::TOKEN_LEN`. Load-bearing beyond auth: quinn's `accept_bi()` does
/// not resolve until the initiator sends bytes, so the preamble is also what
/// registers the tunnel at the relay without deadlock.
const TUNNEL_TOKEN: &[u8] = b"spike-tunnel-token-0123456789abc";

/// The server-minted consumer pairing token (ADR-0051 Decision 4, consumer
/// leg). The relay never reads it; the server side of the tunnel verifies it.
const CONSUMER_TOKEN: &[u8] = b"spike-consumer-token-0123456789a";

/// A token neither leg accepts, for the negative cases.
const WRONG_TOKEN: &[u8] = b"spike-wrong-token-0123456789abcd";

/// Refusal close/reset code, mirroring `transport::quic::AUTH_FAILED_CODE`.
const AUTH_FAILED_CODE: u32 = 0x01;

/// Preamble size bound, mirroring `transport::quic::MAX_TOKEN_PREAMBLE`.
const MAX_TOKEN_PREAMBLE: usize = 256;

// ---------------------------------------------------------------------------
// TLS + endpoint scaffolding (cribbed from phux-client/tests/quic_dial.rs)
// ---------------------------------------------------------------------------

/// A self-signed cert + key in a fresh tempdir, kept alive for the test.
fn cert_pair() -> (TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    phux_server::transport::tls::ensure_self_signed(&cert, &key).unwrap();
    (dir, cert, key)
}

/// The stub relay's quinn server endpoint on an OS-assigned loopback port.
/// It terminates BOTH legs, so it advertises both ALPNs: the dedicated
/// connector protocol and the production consumer protocol. Which leg a
/// connection belongs to is read back from the negotiated ALPN — never from
/// the byte stream.
fn relay_endpoint(cert: &Path, key: &Path) -> (quinn::Endpoint, SocketAddr) {
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
    tls.alpn_protocols = vec![RELAY_ALPN.to_vec(), QUIC_ALPN.to_vec()];
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap();
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    let endpoint = quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = endpoint.local_addr().unwrap();
    (endpoint, addr)
}

/// The ALPN a connection actually negotiated, read from quinn's handshake
/// data. This is how the relay distinguishes the connector leg from consumer
/// legs, and how the test asserts ADR-0051's ALPN separation.
fn negotiated_alpn(conn: &quinn::Connection) -> Vec<u8> {
    conn.handshake_data()
        .expect("handshake completed")
        .downcast::<quinn::crypto::rustls::HandshakeData>()
        .expect("rustls handshake data")
        .protocol
        .expect("an ALPN was negotiated")
}

/// In-test crib of `phux-dial`'s `CertTrust::Pinned` verifier: accept the
/// server certificate iff its SHA-256 leaf fingerprint matches the pin. The
/// connector leg cannot use `phux_dial::quic::dial` (it hardcodes the
/// production consumer ALPN, which invariant 7 forbids on this leg), so the
/// spike hand-rolls the client config — with REAL pinning, not a skip.
#[derive(Debug)]
struct SpikePinnedVerifier {
    provider: Arc<rustls::crypto::CryptoProvider>,
    expected: String,
}

impl rustls::client::danger::ServerCertVerifier for SpikePinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let actual = hex::encode_upper(Sha256::digest(end_entity.as_ref()));
        if actual == self.expected {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "server certificate fingerprint mismatch (pinned {}, got {actual})",
                self.expected
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// The connector's outbound quinn client endpoint: TLS 1.3, the dedicated
/// `phux-relay/1` ALPN, and the pinned-fingerprint verifier. `pin` is in the
/// colon-separated uppercase shape `cert_fingerprint` prints.
fn connector_endpoint(pin: &str) -> quinn::Endpoint {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let expected: String = pin
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let verifier = Arc::new(SpikePinnedVerifier {
        provider: provider.clone(),
        expected,
    });
    let mut crypto = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    crypto.alpn_protocols = vec![RELAY_ALPN.to_vec()];
    let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap();
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(quic_crypto)));
    endpoint
}

// ---------------------------------------------------------------------------
// Preamble + frame helpers
// ---------------------------------------------------------------------------

/// The ADR-0031 auth preamble bytes: `len: u32 BE` + raw token. Matches what
/// `phux_dial::quic::dial` writes, so taps can be compared byte-for-byte.
fn preamble_bytes(token: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(token.len()).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(token);
    out
}

/// Read one auth preamble off a quinn recv stream, bounded like the
/// production reader. Wrapped in [`WIRE_RECV_TIMEOUT`] so a hang fails loudly.
async fn read_preamble(recv: &mut quinn::RecvStream) -> Vec<u8> {
    timeout(WIRE_RECV_TIMEOUT, async {
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf)
            .await
            .expect("preamble length");
        let len = u32::from_be_bytes(len_buf) as usize;
        assert!(
            len <= MAX_TOKEN_PREAMBLE,
            "oversized preamble ({len} bytes)"
        );
        let mut token = vec![0u8; len];
        recv.read_exact(&mut token).await.expect("preamble token");
        token
    })
    .await
    .expect("timed out reading auth preamble")
}

/// Read one length-prefixed phux frame off a quinn recv stream and return
/// the FULL framed bytes (4-byte BE header + body), or the read error — a
/// refused stream (reset) surfaces here as `Err`.
async fn try_read_raw_frame(
    recv: &mut quinn::RecvStream,
) -> Result<Vec<u8>, quinn::ReadExactError> {
    timeout(WIRE_RECV_TIMEOUT, async {
        let mut header = [0u8; 4];
        recv.read_exact(&mut header).await?;
        let len = u32::from_be_bytes(header) as usize;
        let mut framed = header.to_vec();
        framed.resize(4 + len, 0);
        recv.read_exact(&mut framed[4..]).await?;
        Ok(framed)
    })
    .await
    .expect("timed out waiting for frame")
}

// ---------------------------------------------------------------------------
// The stub relay: a rendezvous point, not a peer (ADR-0051 Decision 1)
// ---------------------------------------------------------------------------

/// One bridged consumer's byte taps, tunnel-side stream id attached. The
/// taps record every byte that crossed the relay in each direction, appended
/// BEFORE forwarding, so "what the far side read" is always a prefix of
/// "what the tap holds".
struct BridgedTap {
    tunnel_stream_id: quinn::StreamId,
    c2s: Rc<RefCell<Vec<u8>>>,
    s2c: Rc<RefCell<Vec<u8>>>,
}

/// Everything the test asserts about the relay after the fact. `Rc<RefCell>`
/// is safe here: the whole test runs on a current-thread `LocalSet`
/// (ADR-0003/0014) and no borrow is held across an await.
#[derive(Default)]
struct RelayState {
    tunnel: Option<quinn::Connection>,
    tunnel_alpn: Option<Vec<u8>>,
    tunnel_up: Option<oneshot::Sender<()>>,
    tunnel_rejections: usize,
    consumer_alpns: Vec<Vec<u8>>,
    taps: Vec<BridgedTap>,
    stream0_extra_bytes: bool,
}

impl RelayState {
    fn new(tunnel_up: oneshot::Sender<()>) -> Self {
        Self {
            tunnel_up: Some(tunnel_up),
            ..Self::default()
        }
    }
}

/// Opaque splice half with a byte tap. This IS the relay's entire data
/// path: read, tap, forward. No `FrameKind::decode`, no length-prefix
/// awareness, no ack emission anywhere in the relay's code — ADR-0051
/// invariants 1 and 5 hold by construction, not by discipline.
async fn pump_tapped<R, W>(mut read: R, mut write: W, tap: Rc<RefCell<Vec<u8>>>)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = [0u8; 4096];
    loop {
        let n = match read.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        // Tap BEFORE forwarding (and never across an await): any byte the
        // far side ever observes is already recorded.
        tap.borrow_mut().extend_from_slice(&buf[..n]);
        if write.write_all(&buf[..n]).await.is_err() {
            return;
        }
    }
}

/// Admit (or refuse) the connector's tunnel. The token preamble on stream 0
/// is the relay's OWN handshake — the last bytes it ever parses on this
/// connection (invariant 1).
async fn admit_tunnel(conn: quinn::Connection, alpn: Vec<u8>, state: Rc<RefCell<RelayState>>) {
    let Ok((send0, mut recv0)) = conn.accept_bi().await else {
        return;
    };
    let token = read_preamble(&mut recv0).await;
    if token != TUNNEL_TOKEN {
        state.borrow_mut().tunnel_rejections += 1;
        conn.close(AUTH_FAILED_CODE.into(), b"unauthorized");
        return;
    }
    let tunnel_up = {
        let mut s = state.borrow_mut();
        s.tunnel = Some(conn);
        s.tunnel_alpn = Some(alpn);
        s.tunnel_up.take()
    };
    if let Some(tx) = tunnel_up {
        let _ = tx.send(());
    }
    // Stream-0 watchdog: ADR-0051 Decision 2 reserves stream 0 for the auth
    // preamble ONLY. Any further byte flips the flag the test asserts on.
    // `send0` is held (not dropped) so the reserved stream stays open.
    let _reserved_send0 = send0;
    let mut byte = [0u8; 1];
    if matches!(recv0.read(&mut byte).await, Ok(Some(_))) {
        state.borrow_mut().stream0_extra_bytes = true;
    }
}

/// Bridge one admitted consumer: exactly one FRESH relay-initiated bidi
/// stream toward the connector (ADR-0051 Decision 2 — the load-bearing
/// stream discipline), spliced opaquely in both directions. One finished
/// direction ends the bridge (the stdio-bridge shape).
async fn bridge_consumer(conn: quinn::Connection, alpn: Vec<u8>, state: Rc<RefCell<RelayState>>) {
    let tunnel = {
        let mut s = state.borrow_mut();
        s.consumer_alpns.push(alpn);
        s.tunnel
            .clone()
            .expect("tunnel must be registered before consumers arrive")
    };
    // `accept_bi` resolves once the consumer's first bytes (its bearer
    // preamble) arrive. The relay does NOT read them — they pass through
    // opaquely to the server side of the tunnel (Decision 4).
    let Ok((cons_send, cons_recv)) = conn.accept_bi().await else {
        return;
    };
    let Ok((tun_send, tun_recv)) = tunnel.open_bi().await else {
        return;
    };
    let c2s = Rc::new(RefCell::new(Vec::new()));
    let s2c = Rc::new(RefCell::new(Vec::new()));
    state.borrow_mut().taps.push(BridgedTap {
        tunnel_stream_id: tun_send.id(),
        c2s: Rc::clone(&c2s),
        s2c: Rc::clone(&s2c),
    });
    tokio::select! {
        () = pump_tapped(cons_recv, tun_send, c2s) => {}
        () = pump_tapped(tun_recv, cons_send, s2c) => {}
    }
}

/// Run the stub relay: accept connections forever, sorting each leg by its
/// negotiated ALPN — the connector leg by `phux-relay/1`, consumers by the
/// production ALPN. Owns the endpoint for the test's lifetime.
fn spawn_relay(endpoint: quinn::Endpoint, state: Rc<RefCell<RelayState>>) {
    tokio::task::spawn_local(async move {
        while let Some(incoming) = endpoint.accept().await {
            let state = Rc::clone(&state);
            tokio::task::spawn_local(async move {
                let Ok(conn) = incoming.await else { return };
                let alpn = negotiated_alpn(&conn);
                if alpn == RELAY_ALPN {
                    admit_tunnel(conn, alpn, state).await;
                } else {
                    bridge_consumer(conn, alpn, state).await;
                }
            });
        }
    });
}

// ---------------------------------------------------------------------------
// The connector: the dial-out leg under test
// ---------------------------------------------------------------------------

/// Everything the test asserts about the connector after the fact.
#[derive(Default)]
struct ConnectorState {
    negotiated_alpn: Option<Vec<u8>>,
    own_stream_id: Option<quinn::StreamId>,
    bridged_stream_ids: Vec<quinn::StreamId>,
    bridged: usize,
    rejected_consumers: usize,
}

/// Dial out from "inside the NAT" to the relay, register the tunnel, then
/// serve bridged consumers: verify each consumer's bearer token (the
/// server-side-of-the-tunnel check, ADR-0051 Decision 4) and splice the
/// admitted stream onto a FRESH UDS connection to the server.
fn spawn_connector(
    relay_addr: SocketAddr,
    pin: String,
    socket_path: PathBuf,
    state: Rc<RefCell<ConnectorState>>,
) {
    tokio::task::spawn_local(async move {
        let endpoint = connector_endpoint(&pin);
        let conn = endpoint
            .connect(relay_addr, "localhost")
            .unwrap()
            .await
            .expect("connector leg establishes to the relay (pin + ALPN)");
        state.borrow_mut().negotiated_alpn = Some(negotiated_alpn(&conn));

        // Stream 0: the connector-initiated control stream. It carries ONLY
        // the tunnel auth preamble and is held open, reserved (Decision 2).
        let (mut send0, recv0) = conn.open_bi().await.unwrap();
        state.borrow_mut().own_stream_id = Some(send0.id());
        send0
            .write_all(&preamble_bytes(TUNNEL_TOKEN))
            .await
            .unwrap();
        let _reserved_stream0 = (send0, recv0);

        // Every bridged consumer arrives as a fresh RELAY-initiated stream.
        while let Ok((mut tun_send, mut tun_recv)) = conn.accept_bi().await {
            state.borrow_mut().bridged_stream_ids.push(tun_send.id());
            // The consumer's own bearer token crossed the relay opaquely;
            // verify it HERE, before any byte reaches the server. Relay
            // admission is not authorization. (Production: the server's
            // `TokenStore` on the QUIC accept path — spike tolerance, see
            // the header comment.)
            let token = read_preamble(&mut tun_recv).await;
            if token != CONSUMER_TOKEN {
                state.borrow_mut().rejected_consumers += 1;
                let _ = tun_send.reset(AUTH_FAILED_CODE.into());
                let _ = tun_recv.stop(AUTH_FAILED_CODE.into());
                continue;
            }
            state.borrow_mut().bridged += 1;
            // Fresh UDS connection per admitted consumer, spliced with the
            // stdio-bridge shape: one finished direction ends the bridge.
            let socket_path = socket_path.clone();
            tokio::task::spawn_local(async move {
                let uds = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
                let (mut from_server, mut to_server) = uds.into_split();
                tokio::select! {
                    _ = tokio::io::copy(&mut tun_recv, &mut to_server) => {}
                    _ = tokio::io::copy(&mut from_server, &mut tun_send) => {}
                }
            });
        }
    });
}

// ---------------------------------------------------------------------------
// Consumer helpers (raw wire, so every sent/received byte is accounted for)
// ---------------------------------------------------------------------------

/// A consumer leg with full byte accounting: `sent` is every byte written to
/// the relay (preamble included), `recv_raw` every framed byte read back.
struct Consumer {
    /// Keeps the I/O driver alive for the connection's lifetime.
    _endpoint: quinn::Endpoint,
    conn: quinn::Connection,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    sent: Vec<u8>,
    recv_raw: Vec<u8>,
}

/// Dial the relay as a consumer via the PRODUCTION dialer, untouched:
/// `phux-quic/1` ALPN, pinned fingerprint, bearer-token preamble.
async fn dial_consumer(relay_addr: SocketAddr, pin: &str, token: &[u8]) -> Consumer {
    let dial = QuicDial {
        addr: relay_addr,
        server_name: "localhost".to_owned(),
        token: Some(token.to_vec()),
        trust: CertTrust::Pinned(pin.to_owned()),
    };
    let (endpoint, conn, send, recv) = phux_dial::quic::dial(&dial)
        .await
        .expect("consumer dials the relay through the production dialer");
    Consumer {
        _endpoint: endpoint,
        conn,
        send,
        recv,
        // `dial` already wrote the preamble; account for it.
        sent: preamble_bytes(token),
        recv_raw: Vec::new(),
    }
}

async fn send_wire(consumer: &mut Consumer, frame: &FrameKind) {
    let buf = encode_frame(frame);
    consumer.send.write_all(&buf).await.unwrap();
    consumer.sent.extend_from_slice(&buf);
}

/// Read + decode one frame, recording the raw bytes. The full-consumption
/// check proves the 4-byte length-prefix framing survived two QUIC hops and
/// the UDS splice intact.
async fn recv_wire(consumer: &mut Consumer) -> FrameKind {
    let framed = try_read_raw_frame(&mut consumer.recv)
        .await
        .expect("read frame through the relay");
    consumer.recv_raw.extend_from_slice(&framed);
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
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: ClientCapabilities::default(),
    }
}

/// Dial + full HELLO -> ATTACH -> ATTACHED + `TERMINAL_SNAPSHOT` handshake
/// through both legs, under the default client timeouts. Returns the
/// consumer and the attached pane's wire id.
async fn connect_and_attach(
    relay_addr: SocketAddr,
    pin: &str,
    client_name: &str,
) -> (Consumer, phux_protocol::ids::TerminalId) {
    let mut consumer = dial_consumer(relay_addr, pin, CONSUMER_TOKEN).await;
    send_wire(&mut consumer, &hello(client_name)).await;
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
    (consumer, pane_id.unwrap())
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

/// Drain `TERMINAL_OUTPUT` frames until `needle` appears in the accumulated
/// VT bytes or the deadline elapses (crib of `input_dispatch::await_echo`).
async fn await_echo(consumer: &mut Consumer, needle: u8) {
    let mut acc: Vec<u8> = Vec::new();
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if let FrameKind::TerminalOutput { bytes, .. } = recv_wire(consumer).await {
            acc.extend_from_slice(&bytes);
            if acc.contains(&needle) {
                return;
            }
        }
    }
    panic!(
        "INPUT_KEY must round-trip through relay + tunnel to the PTY and echo back \
         as TERMINAL_OUTPUT (got {} bytes: {acc:?})",
        acc.len()
    );
}

// ---------------------------------------------------------------------------
// The spike
// ---------------------------------------------------------------------------

/// The happy path, end to end (ADR-0051 assertions 1-6): a NAT'd server is
/// reached by two concurrent consumers through the relay the connector
/// dialed out to — full handshakes, live PTY output, byte-identical taps,
/// ALPN separation, and the per-consumer stream discipline.
#[test]
#[allow(clippy::too_many_lines, reason = "one linear protocol scenario")]
fn consumers_attach_through_relay_to_dialed_out_server() {
    run_local(async {
        // NAT invariant: the server's ONLY listener is this UDS in a
        // tempdir. No `.listen_ws` / `.listen_quic`, no `PHUX_*_ADDR` env.
        // The only network socket accepting inbound connections below is
        // the relay's; the server side only ever dials OUT.
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        // `cat` is the deterministic PTY echo fixture (assertion 5's live
        // output), mirroring input_dispatch.rs.
        let cmd = portable_pty::CommandBuilder::new("/bin/cat");
        let (shutdown, server) = spawn_server_with_seed_cmd(socket_path.clone(), "default", cmd);

        let (_cert_dir, cert, key) = cert_pair();
        let (relay_ep, relay_addr) = relay_endpoint(&cert, &key);
        let pin = phux_server::transport::tls::cert_fingerprint(&cert).unwrap();

        let (tunnel_up_tx, tunnel_up_rx) = oneshot::channel();
        let relay_state = Rc::new(RefCell::new(RelayState::new(tunnel_up_tx)));
        spawn_relay(relay_ep, Rc::clone(&relay_state));

        let connector_state = Rc::new(RefCell::new(ConnectorState::default()));
        spawn_connector(
            relay_addr,
            pin.clone(),
            socket_path.clone(),
            Rc::clone(&connector_state),
        );

        // NAT-ordering proof: the outbound tunnel registers at the relay
        // while zero consumers exist.
        timeout(SOCKET_CONNECT_DEADLINE, tunnel_up_rx)
            .await
            .expect("tunnel must register at the relay before any consumer exists")
            .expect("relay dropped the tunnel_up signal");

        // Two CONCURRENT consumers, each completing an independent
        // handshake (assertion 3: no cross-stream corruption).
        let ((mut consumer_a, pane_a), (consumer_b, _pane_b)) = tokio::join!(
            connect_and_attach(relay_addr, &pin, "relay-spike-consumer-a"),
            connect_and_attach(relay_addr, &pin, "relay-spike-consumer-b"),
        );

        // Live PTY output (assertion 5): consumer A types 'a' + Enter into
        // the cat pane; the echo must come back as TERMINAL_OUTPUT frames
        // whose byte-identity is then covered by the taps below.
        send_wire(
            &mut consumer_a,
            &FrameKind::InputKey {
                terminal_id: pane_a.clone(),
                event: ascii_key('a', PhysicalKey::A),
            },
        )
        .await;
        send_wire(
            &mut consumer_a,
            &FrameKind::InputKey {
                terminal_id: pane_a,
                event: enter_key(),
            },
        )
        .await;
        await_echo(&mut consumer_a, b'a').await;

        // ---- ALPN separation (assertion 2) ----
        {
            let cs = connector_state.borrow();
            assert_eq!(
                cs.negotiated_alpn.as_deref(),
                Some(RELAY_ALPN),
                "connector leg negotiates the dedicated relay ALPN"
            );
        }
        {
            let rs = relay_state.borrow();
            assert_eq!(
                rs.tunnel_alpn.as_deref(),
                Some(RELAY_ALPN),
                "relay saw the tunnel arrive under phux-relay/1"
            );
            assert_eq!(
                rs.consumer_alpns,
                vec![QUIC_ALPN.to_vec(), QUIC_ALPN.to_vec()],
                "both consumer legs negotiated the production ALPN"
            );
        }
        assert_eq!(negotiated_alpn(&consumer_a.conn), QUIC_ALPN);
        assert_eq!(negotiated_alpn(&consumer_b.conn), QUIC_ALPN);
        assert_ne!(
            RELAY_ALPN, QUIC_ALPN,
            "the two legs negotiate distinct protocols (invariant 7)"
        );

        // ---- Stream discipline (assertion 3) ----
        {
            let cs = connector_state.borrow();
            let own = cs.own_stream_id.expect("connector opened stream 0");
            assert_eq!(u64::from(own), 0, "connector control stream is stream 0");
            assert_eq!(own.initiator(), quinn::Side::Client);
            assert_eq!(
                cs.bridged_stream_ids.len(),
                2,
                "one bridged stream per consumer"
            );
            for id in &cs.bridged_stream_ids {
                assert_eq!(
                    id.initiator(),
                    quinn::Side::Server,
                    "every consumer stream is RELAY-initiated"
                );
                assert_eq!(id.dir(), quinn::Dir::Bi);
                assert_ne!(u64::from(*id), 0, "consumers never ride stream 0");
            }
            assert_ne!(
                cs.bridged_stream_ids[0], cs.bridged_stream_ids[1],
                "each consumer got its own fresh stream"
            );
            assert_eq!(cs.bridged, 2, "both consumers were admitted and spliced");
            assert_eq!(cs.rejected_consumers, 0);
        }
        {
            let rs = relay_state.borrow();
            assert!(
                !rs.stream0_extra_bytes,
                "stream 0 carried ONLY the connector's auth preamble"
            );
            assert_eq!(rs.taps.len(), 2, "the relay bridged exactly two streams");
            let cs = connector_state.borrow();
            for tap in &rs.taps {
                assert!(
                    cs.bridged_stream_ids.contains(&tap.tunnel_stream_id),
                    "relay-side and connector-side stream ids agree"
                );
            }
        }

        // ---- Two-token pass-through + byte-identity (assertions 4 and 6) ----
        // Each consumer's every sent byte (bearer preamble included) crossed
        // the relay byte-identical, in order, nothing added or dropped; and
        // every byte a consumer read is a prefix of what entered the relay
        // from the tunnel (the server may keep emitting after the consumer
        // stops reading). Matching taps by content also proves the two
        // streams never cross-contaminated.
        {
            let rs = relay_state.borrow();
            for consumer in [&consumer_a, &consumer_b] {
                let matched = rs
                    .taps
                    .iter()
                    .find(|t| *t.c2s.borrow() == consumer.sent)
                    .expect("a bridged stream carried this consumer's exact bytes");
                let s2c = matched.s2c.borrow();
                assert!(!consumer.recv_raw.is_empty());
                assert!(
                    s2c.len() >= consumer.recv_raw.len(),
                    "tap is appended before delivery, so it can never lag the reader"
                );
                assert_eq!(
                    &s2c[..consumer.recv_raw.len()],
                    &consumer.recv_raw[..],
                    "server-to-consumer bytes are bit-identical through the relay"
                );
            }
        }

        // Clean teardown: prompt CONNECTION_CLOSE on both consumers, then
        // server shutdown. Remaining local tasks (relay, connector) are
        // dropped when run_local returns.
        consumer_a.conn.close(0u32.into(), b"done");
        consumer_b.conn.close(0u32.into(), b"done");
        shutdown.send(()).ok();
        timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, server)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}

/// Negative half of assertion 1: the relay refuses a connector presenting a
/// wrong tunnel token — an application close with the auth code, and no
/// tunnel ever registers.
#[test]
fn relay_refuses_connector_with_wrong_tunnel_token() {
    run_local(async {
        let (_cert_dir, cert, key) = cert_pair();
        let (relay_ep, relay_addr) = relay_endpoint(&cert, &key);
        let pin = phux_server::transport::tls::cert_fingerprint(&cert).unwrap();
        let (tunnel_up_tx, mut tunnel_up_rx) = oneshot::channel();
        let state = Rc::new(RefCell::new(RelayState::new(tunnel_up_tx)));
        spawn_relay(relay_ep, Rc::clone(&state));

        let endpoint = connector_endpoint(&pin);
        let conn = endpoint
            .connect(relay_addr, "localhost")
            .unwrap()
            .await
            .expect("TLS + ALPN succeed; the refusal is at the token layer");
        let (mut send0, _recv0) = conn.open_bi().await.unwrap();
        send0.write_all(&preamble_bytes(WRONG_TOKEN)).await.unwrap();

        let closed = timeout(SOCKET_CONNECT_DEADLINE, conn.closed())
            .await
            .expect("relay must close the connection promptly, not idle it out");
        match closed {
            quinn::ConnectionError::ApplicationClosed(app) => assert_eq!(
                app.error_code,
                quinn::VarInt::from(AUTH_FAILED_CODE),
                "refusal carries the auth-failed code"
            ),
            other => panic!("expected an application close from the relay, got {other:?}"),
        }
        {
            let s = state.borrow();
            assert_eq!(s.tunnel_rejections, 1);
            assert!(
                s.tunnel.is_none(),
                "a refused connector never registers a tunnel"
            );
        }
        assert!(
            tunnel_up_rx.try_recv().is_err(),
            "tunnel_up must never signal for a refused connector"
        );
    });
}

/// The other fail-closed direction of assertion 1: a relay whose certificate
/// does not match the connector's pin is refused by the connector before any
/// token is sent.
#[test]
fn connector_refuses_relay_with_wrong_cert_fingerprint() {
    run_local(async {
        let (_cert_dir, cert, key) = cert_pair();
        let (relay_ep, relay_addr) = relay_endpoint(&cert, &key);
        // Drive the server accept on a detached task; it never yields a
        // connection because the connector aborts at cert verification.
        let accept = tokio::task::spawn_local(async move {
            if let Some(incoming) = relay_ep.accept().await {
                let _ = incoming.await;
            }
        });

        // A 32-byte all-zero fingerprint cannot match the real leaf.
        let endpoint = connector_endpoint(&"00".repeat(32));
        let result = timeout(
            SOCKET_CONNECT_DEADLINE,
            endpoint.connect(relay_addr, "localhost").unwrap(),
        )
        .await
        .expect("handshake must fail fast, not hang");
        assert!(
            result.is_err(),
            "a mismatched certificate pin must fail the connector leg closed"
        );
        accept.abort();
    });
}

/// The killer assertion's negative half (assertion 4): a consumer the relay
/// ADMITS — TLS, ALPN, and bridging all succeed — but whose bearer token the
/// server side of the tunnel rejects is refused, proving relay admission is
/// not authorization. The tunnel survives the refusal: a well-tokened
/// consumer attaches immediately afterwards.
#[test]
fn relay_admission_is_not_authorization_for_consumers() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server(socket_path.clone(), Some("default"));

        let (_cert_dir, cert, key) = cert_pair();
        let (relay_ep, relay_addr) = relay_endpoint(&cert, &key);
        let pin = phux_server::transport::tls::cert_fingerprint(&cert).unwrap();
        let (tunnel_up_tx, tunnel_up_rx) = oneshot::channel();
        let relay_state = Rc::new(RefCell::new(RelayState::new(tunnel_up_tx)));
        spawn_relay(relay_ep, Rc::clone(&relay_state));
        let connector_state = Rc::new(RefCell::new(ConnectorState::default()));
        spawn_connector(
            relay_addr,
            pin.clone(),
            socket_path.clone(),
            Rc::clone(&connector_state),
        );
        timeout(SOCKET_CONNECT_DEADLINE, tunnel_up_rx)
            .await
            .expect("tunnel up")
            .expect("tunnel_up signal");

        // The bad consumer presents a token the relay cannot judge (it
        // never parses it) and the server side rejects.
        let mut bad = dial_consumer(relay_addr, &pin, WRONG_TOKEN).await;
        send_wire(&mut bad, &hello("relay-spike-bad-token")).await;
        send_wire(&mut bad, &attach_by_name("default")).await;
        let refused = try_read_raw_frame(&mut bad.recv).await;
        assert!(
            refused.is_err(),
            "the server side of the tunnel must refuse the stream before any frame flows, got {refused:?}"
        );

        // Admission happened — the relay bridged the stream opaquely —
        // and authorization was denied downstream of it.
        assert_eq!(
            relay_state.borrow().taps.len(),
            1,
            "the relay admitted and bridged the bad consumer"
        );
        {
            let cs = connector_state.borrow();
            assert_eq!(cs.rejected_consumers, 1, "the connector refused the token");
            assert_eq!(
                cs.bridged, 0,
                "no UDS splice was opened for the rejected consumer"
            );
        }
        bad.conn.close(0u32.into(), b"done");

        // The tunnel survives a per-consumer refusal: a well-tokened
        // consumer completes the full handshake right after.
        let (good, _pane) = connect_and_attach(relay_addr, &pin, "relay-spike-good-token").await;
        assert_eq!(connector_state.borrow().bridged, 1);
        assert_eq!(connector_state.borrow().rejected_consumers, 1);

        good.conn.close(0u32.into(), b"done");
        shutdown.send(()).ok();
        timeout(phux_server_testkit::SERVER_JOIN_DEADLINE, server)
            .await
            .expect("server did not shut down after the shutdown signal")
            .expect("server join")
            .expect("server run_async ok");
    });
}
