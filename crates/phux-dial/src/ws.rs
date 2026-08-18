//! Native WebSocket dial transport.
//!
//! This is the TCP fallback sibling to QUIC: one binary WebSocket message
//! carries one complete length-prefixed phux frame, matching the server's
//! `WsListener` and the browser client. This module owns establishment only
//! (TCP connect, optional TLS with the shared trust policy, RFC 6455 upgrade
//! with the `Authorization: Bearer` pairing token); message framing stays
//! with the callers via [`WsReader`] / [`WsWriter`].
//!
//! It also owns this lane's **liveness**. QUIC gets peer-death detection for
//! free from its transport config (`quic::KEEP_ALIVE` / `quic::IDLE_TIMEOUT`);
//! TCP does not. A laptop that switches networks — wifi to cellular, or wifi
//! dropped and rejoined on a new AP — leaves the old socket with no FIN and no
//! RST, so a `wss://` read parks forever and the client hangs instead of
//! reconnecting. [`WsKeepalive`] closes that gap with RFC 6455 ping/pong at
//! the same 10s/30s cadence the QUIC lane uses, and
//! [`recv_message_alive`] is the read path that applies it.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message};
use tokio_tungstenite::{WebSocketStream, client_async};

use crate::DialError;
use crate::tls::CertTrust;

/// How often an otherwise silent WebSocket sends a client-initiated RFC 6455
/// ping, matched to `quic::KEEP_ALIVE` so both remote lanes behave the same.
///
/// Client-initiated rather than server-initiated on purpose: every RFC 6455
/// peer must answer a ping with a pong, and tungstenite does so automatically
/// on its read path. So this detects a stalled link against **unmodified**
/// phux servers — no wire change, no version negotiation, no skew window.
pub const WS_PING_INTERVAL: Duration = Duration::from_secs(10);

/// How long a WebSocket may go without *any* inbound message before the peer
/// is declared gone.
///
/// A frame, a pong, or a peer-initiated ping all count. Matched to
/// `quic::IDLE_TIMEOUT`, and three ping intervals wide so a single dropped
/// keepalive on a lossy link is not mistaken for a dead peer.
pub const WS_LIVENESS_TIMEOUT: Duration = Duration::from_secs(30);

/// What the keepalive wants done next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsLiveness {
    /// Nothing is due; wait at most this long before asking again.
    Idle(Duration),
    /// Send a ping now.
    Ping,
    /// Nothing has arrived within the liveness timeout — the peer is gone.
    Dead,
}

/// Ping/pong liveness policy for one WebSocket, as a pure state machine.
///
/// Deliberately clock-injected (`now` is a parameter, never read internally)
/// so the policy is exercised by ordinary unit tests rather than by sleeping.
///
/// **Any** inbound message counts as proof of life, not just a pong: a pong
/// for our ping, a peer-initiated ping, and an ordinary phux frame are all
/// bytes that traversed the path, which is exactly the question being asked.
/// That also means a busy session never pings at all — the keepalive only
/// costs anything on an idle connection.
#[derive(Debug, Clone, Copy)]
pub struct WsKeepalive {
    ping_interval: Duration,
    liveness_timeout: Duration,
    last_inbound: Instant,
    /// When the last ping went out, reset to `None` by inbound traffic so a
    /// live connection re-arms from scratch.
    last_ping: Option<Instant>,
}

impl WsKeepalive {
    /// The production policy: [`WS_PING_INTERVAL`] / [`WS_LIVENESS_TIMEOUT`].
    #[must_use]
    pub const fn new(now: Instant) -> Self {
        Self::with_timings(now, WS_PING_INTERVAL, WS_LIVENESS_TIMEOUT)
    }

    /// The policy with explicit timings. Tests use this to compress a 30s
    /// window into milliseconds; production uses [`Self::new`].
    #[must_use]
    pub const fn with_timings(
        now: Instant,
        ping_interval: Duration,
        liveness_timeout: Duration,
    ) -> Self {
        Self {
            ping_interval,
            liveness_timeout,
            last_inbound: now,
            last_ping: None,
        }
    }

    /// Record that something arrived from the peer.
    pub const fn note_inbound(&mut self, now: Instant) {
        self.last_inbound = now;
        self.last_ping = None;
    }

    /// Record that a ping was handed to the sink.
    pub const fn note_ping(&mut self, now: Instant) {
        self.last_ping = Some(now);
    }

    /// What to do at `now`.
    #[must_use]
    pub fn poll(&self, now: Instant) -> WsLiveness {
        let dead_at = self.last_inbound + self.liveness_timeout;
        if now >= dead_at {
            return WsLiveness::Dead;
        }
        let ping_at = self.last_ping.unwrap_or(self.last_inbound) + self.ping_interval;
        if now >= ping_at {
            return WsLiveness::Ping;
        }
        // Cap the nap at the death deadline: a ping sent late in the window
        // schedules its successor past `dead_at`, and sleeping to *that*
        // would let a dead peer outlive its own timeout.
        WsLiveness::Idle(ping_at.min(dead_at) - now)
    }
}

/// A native WebSocket remote dial target.
#[derive(Debug, Clone)]
pub struct WsDial {
    /// `ws://` or `wss://` URL for a `phux server --listen` endpoint.
    pub url: String,
    /// Optional hex pairing token, sent as `Authorization: Bearer`.
    pub token: Option<String>,
    /// TLS trust mode. Only used for `wss://`.
    pub trust: CertTrust,
    /// Optional TLS server name override for SNI/certificate verification.
    pub tls_server_name: Option<String>,
}

/// The established WebSocket stream type [`dial`] returns.
pub type Ws = WebSocketStream<ClientStream>;

/// The plain-or-TLS TCP stream underneath the WebSocket.
#[derive(Debug)]
pub enum ClientStream {
    /// Plaintext TCP (`ws://`, loopback dev only).
    Plain(TcpStream),
    /// TLS over TCP (`wss://`), trust per [`CertTrust`].
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl tokio::io::AsyncRead for ClientStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for ClientStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_flush(cx),
            Self::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

/// Connect to the WebSocket listener: TCP connect, optional TLS handshake,
/// then the RFC 6455 upgrade with the bearer token attached when present.
///
/// # Errors
///
/// Returns [`DialError::Unreachable`] when the host's name does not resolve
/// or the TCP connect gets no answer (refused, no route, timed out),
/// [`DialError::Connect`] on other connect and TLS/upgrade failures
/// (including a fingerprint that did not match the pin), and
/// [`DialError::Io`] on tungstenite-level socket I/O failures during the
/// upgrade.
pub async fn dial(d: &WsDial) -> Result<Ws, DialError> {
    let target = WsTarget::parse(&d.url)?;
    // Resolve explicitly first: a name that does not resolve is a
    // reachability failure, not a generic connect failure — on an overlay
    // network, MagicDNS being down (Tailscale stopped on this end) fails
    // exactly here. The connect below re-resolves the same (host, port)
    // tuple, which after a successful lookup is a cheap cache hit and keeps
    // one connect path that still tries every resolved address.
    if let Err(err) = tokio::net::lookup_host((target.host.as_str(), target.port)).await {
        return Err(DialError::Unreachable(format!(
            "dial {}: name resolution failed: {err}",
            target.addr_label()
        )));
    }
    let tcp = TcpStream::connect((target.host.as_str(), target.port))
        .await
        .map_err(|err| {
            let msg = format!("dial {}: {err}", target.addr_label());
            if crate::is_reachability_io(&err) {
                DialError::Unreachable(msg)
            } else {
                DialError::Connect(msg)
            }
        })?;
    let stream = if target.secure {
        ClientStream::Tls(Box::new(tls_connect(tcp, &target, d).await?))
    } else {
        ClientStream::Plain(tcp)
    };

    let mut req = d
        .url
        .as_str()
        .into_client_request()
        .map_err(|err| DialError::Connect(format!("build WebSocket request: {err}")))?;
    if let Some(token) = &d.token {
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {}", token.trim())
                .parse()
                .map_err(|err| DialError::Connect(format!("build Authorization header: {err}")))?,
        );
    }

    client_async(req, stream)
        .await
        .map(|(ws, _)| ws)
        .map_err(ws_error)
}

async fn tls_connect(
    tcp: TcpStream,
    target: &WsTarget,
    dial: &WsDial,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, DialError> {
    let config = Arc::new(crate::tls::client_config(&dial.trust, None)?);
    let connector = tokio_rustls::TlsConnector::from(config);
    let server_name = dial
        .tls_server_name
        .clone()
        .unwrap_or_else(|| target.server_name.clone());
    let server_name = rustls::pki_types::ServerName::try_from(server_name)
        .map_err(|err| DialError::Connect(format!("invalid TLS server name: {err}")))?;
    connector
        .connect(server_name, tcp)
        .await
        .map_err(|err| DialError::Connect(format!("TLS handshake with {}: {err}", target.host)))
}

fn ws_error(err: TungsteniteError) -> DialError {
    match err {
        TungsteniteError::Io(err) => DialError::Io(err),
        other => DialError::Connect(format!("WebSocket handshake: {other}")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Parsed WebSocket remote dial endpoint.
pub struct WsTarget {
    /// Whether the URL uses `wss://`.
    pub secure: bool,
    /// TCP destination host from the URL.
    pub host: String,
    /// TCP destination port, including scheme defaults.
    pub port: u16,
    /// Hostname used as the default TLS server name.
    pub server_name: String,
}

impl WsTarget {
    /// Parse a `ws://` or `wss://` dial URL.
    ///
    /// # Errors
    ///
    /// Returns [`DialError::Connect`] for a malformed URL, a missing host, or
    /// a non-WebSocket scheme.
    pub fn parse(raw_url: &str) -> Result<Self, DialError> {
        let parsed: Uri = raw_url
            .parse()
            .map_err(|err| DialError::Connect(format!("invalid WebSocket URL: {err}")))?;
        let scheme = parsed
            .scheme_str()
            .ok_or_else(|| DialError::Connect("WebSocket URL is missing a scheme".to_owned()))?;
        let secure = match scheme {
            "ws" => false,
            "wss" => true,
            _ => {
                return Err(DialError::Connect(
                    "WebSocket URL must start with ws:// or wss://".to_owned(),
                ));
            }
        };
        let host = parsed
            .host()
            .ok_or_else(|| DialError::Connect("WebSocket URL is missing a host".to_owned()))?
            .to_owned();
        let port = parsed.port_u16().unwrap_or(if secure { 443 } else { 80 });
        Ok(Self {
            secure,
            server_name: host.trim_matches(['[', ']']).to_owned(),
            host,
            port,
        })
    }

    /// Whether the URL host is loopback-only.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        let host = self.server_name.as_str();
        host.eq_ignore_ascii_case("localhost")
            || host.parse::<IpAddr>().is_ok_and(|addr| addr.is_loopback())
    }

    fn addr_label(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Read half of an established WebSocket: one binary message per phux frame.
#[derive(Debug)]
pub struct WsReader {
    /// The message stream half from [`futures_util::StreamExt::split`].
    pub rx: futures_util::stream::SplitStream<Ws>,
    /// Liveness state for this connection, advanced by
    /// [`recv_message_alive`]. Carried on the reader rather than in a caller
    /// local because the attach driver reads inside a `tokio::select!`: the
    /// read future is dropped and rebuilt on every keystroke, and a deadline
    /// living in a local would restart with it.
    keepalive: WsKeepalive,
}

/// Write half of an established WebSocket.
#[derive(Debug)]
pub struct WsWriter {
    /// The message sink half from [`futures_util::StreamExt::split`].
    pub tx: futures_util::stream::SplitSink<Ws, Message>,
}

impl WsWriter {
    /// Send one already-encoded phux frame as a single binary message.
    ///
    /// # Errors
    ///
    /// Propagates transport failures as [`DialError`].
    pub async fn send(&mut self, frame: &[u8]) -> Result<(), DialError> {
        self.tx
            .send(Message::Binary(frame.to_vec()))
            .await
            .map_err(ws_error)
    }

    /// Send an RFC 6455 ping. The payload is empty: this asks "are you
    /// there", and the answer is the pong arriving at all, not its contents.
    ///
    /// # Errors
    ///
    /// Propagates transport failures as [`DialError`].
    pub async fn send_ping(&mut self) -> Result<(), DialError> {
        self.tx
            .send(Message::Ping(Vec::new()))
            .await
            .map_err(ws_error)
    }
}

impl WsReader {
    /// Wrap a split stream half with the production liveness policy.
    #[must_use]
    pub fn new(rx: futures_util::stream::SplitStream<Ws>) -> Self {
        Self {
            rx,
            keepalive: WsKeepalive::new(Instant::now()),
        }
    }

    /// Wrap a split stream half with explicit keepalive timings (tests).
    #[must_use]
    pub const fn with_keepalive(
        rx: futures_util::stream::SplitStream<Ws>,
        keepalive: WsKeepalive,
    ) -> Self {
        Self { rx, keepalive }
    }

    /// Receive the next binary message, skipping control frames.
    ///
    /// Returns `Ok(None)` on a clean close.
    ///
    /// **This read can park forever.** A half-open TCP connection — the
    /// laptop-changed-networks case — never delivers EOF or an error, so
    /// nothing here will ever return. Session code wants
    /// [`recv_message_alive`], which bounds the wait with ping/pong; this
    /// plain form is for exchanges already bounded by the caller.
    ///
    /// # Errors
    ///
    /// Propagates transport failures as [`DialError`].
    pub async fn recv_message(&mut self) -> Result<Option<Vec<u8>>, DialError> {
        loop {
            match self.rx.next().await {
                None | Some(Ok(Message::Close(_))) => return Ok(None),
                Some(Ok(Message::Binary(data))) => {
                    self.keepalive.note_inbound(Instant::now());
                    return Ok(Some(data));
                }
                Some(Err(err)) => return Err(ws_error(err)),
                // Text / ping / pong / raw: not a phux frame, but proof the
                // path is still carrying bytes. tungstenite has already
                // queued the automatic pong for a peer ping.
                Some(Ok(_)) => self.keepalive.note_inbound(Instant::now()),
            }
        }
    }
}

/// Receive the next phux frame, keeping the connection alive and reporting a
/// stalled peer instead of waiting on it forever.
///
/// Takes both halves because RFC 6455 liveness is inherently full-duplex: the
/// question is asked on the sink and answered on the stream. Splitting the
/// socket is what the framed reader/writer seam requires, so the loop that
/// spans the split lives here rather than in either half.
///
/// While frames are flowing this is exactly [`WsReader::recv_message`] —
/// inbound traffic is itself proof of life, so a busy session never pings.
/// After [`WS_PING_INTERVAL`] of silence it pings; after
/// [`WS_LIVENESS_TIMEOUT`] with nothing back it returns
/// [`DialError::Stalled`].
///
/// # Cancel safety
///
/// Safe to drop and re-enter, which the attach driver does on every keystroke.
/// All deadline state lives on `reader`, and the ping is recorded before it is
/// awaited, so a cancellation mid-send cannot turn into a ping storm.
///
/// # Errors
///
/// [`DialError::Stalled`] when the peer stops answering; otherwise the
/// transport failures [`WsReader::recv_message`] surfaces.
pub async fn recv_message_alive(
    reader: &mut WsReader,
    writer: &mut WsWriter,
) -> Result<Option<Vec<u8>>, DialError> {
    loop {
        let nap = match reader.keepalive.poll(Instant::now()) {
            WsLiveness::Dead => {
                return Err(DialError::Stalled(format!(
                    "no WebSocket traffic for {}s and no pong for our keepalive ping",
                    reader.keepalive.liveness_timeout.as_secs()
                )));
            }
            WsLiveness::Ping => {
                // Recorded before the await: a `select!` that cancels this
                // future mid-send must not re-enter and ping again.
                reader.keepalive.note_ping(Instant::now());
                writer.send_ping().await?;
                continue;
            }
            WsLiveness::Idle(nap) => nap,
        };
        match tokio::time::timeout(nap, reader.recv_message()).await {
            Ok(result) => return result,
            // The nap elapsed: loop back and let `poll` decide whether that
            // means "ping" or "dead".
            Err(_elapsed) => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn parses_ws_and_wss_targets() {
        let ws = WsTarget::parse("ws://127.0.0.1:8787/path").expect("ws");
        assert_eq!(ws.host, "127.0.0.1");
        assert_eq!(ws.port, 8787);
        assert!(!ws.secure);
        assert!(ws.is_loopback());

        let wss = WsTarget::parse("wss://example.com/phux").expect("wss");
        assert_eq!(wss.host, "example.com");
        assert_eq!(wss.port, 443);
        assert!(wss.secure);
        assert!(!wss.is_loopback());
    }

    #[test]
    fn rejects_non_websocket_scheme() {
        assert!(WsTarget::parse("https://example.com/").is_err());
    }

    #[tokio::test]
    async fn refused_tcp_connect_classifies_unreachable() {
        // Bind then drop a listener so the port is known-refusing.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);

        let err = dial(&WsDial {
            url: format!("ws://127.0.0.1:{port}"),
            token: None,
            trust: CertTrust::SkipVerify,
            tls_server_name: None,
        })
        .await
        .expect_err("nothing is listening");
        assert!(matches!(err, DialError::Unreachable(_)), "got {err:?}");
        assert!(
            err.to_string()
                .starts_with("transport connect error: dial 127.0.0.1:"),
            "got {err}"
        );
    }

    /// A hostname that does not resolve classifies as `Unreachable` — the
    /// `MagicDNS`-down shape of an overlay outage. `.invalid` is reserved by
    /// RFC 2606 and guaranteed never to resolve.
    #[tokio::test]
    async fn unresolvable_hostname_classifies_unreachable() {
        let err = dial(&WsDial {
            url: "ws://phux-test-nxdomain.invalid:8787".to_owned(),
            token: None,
            trust: CertTrust::SkipVerify,
            tls_server_name: None,
        })
        .await
        .expect_err(".invalid never resolves");
        assert!(matches!(err, DialError::Unreachable(_)), "got {err:?}");
        assert!(
            err.to_string().contains("name resolution failed"),
            "got {err}"
        );
    }

    // ---- keepalive policy ------------------------------------------------

    /// Compressed test timings: the production 10s/30s shape, 100x faster.
    const TEST_PING: Duration = Duration::from_millis(100);
    const TEST_DEAD: Duration = Duration::from_millis(300);

    /// The steady state of a busy session: traffic keeps arriving, so nothing
    /// is ever due and the keepalive costs exactly one comparison per read.
    #[test]
    fn traffic_keeps_the_keepalive_quiet() {
        let start = Instant::now();
        let mut keepalive = WsKeepalive::with_timings(start, TEST_PING, TEST_DEAD);

        for step in 1..10 {
            let now = start + TEST_PING / 2 * step;
            assert!(
                matches!(keepalive.poll(now), WsLiveness::Idle(_)),
                "inbound traffic every half-interval never needs a ping"
            );
            keepalive.note_inbound(now);
        }
    }

    /// Silence walks the state machine: quiet -> ping -> quiet again while we
    /// wait for the answer -> dead once the whole window has elapsed with
    /// nothing back.
    #[test]
    fn silence_pings_then_declares_the_peer_dead() {
        let start = Instant::now();
        let mut keepalive = WsKeepalive::with_timings(start, TEST_PING, TEST_DEAD);

        assert_eq!(
            keepalive.poll(start),
            WsLiveness::Idle(TEST_PING),
            "a fresh connection waits a full interval before probing"
        );
        assert_eq!(keepalive.poll(start + TEST_PING), WsLiveness::Ping);

        // Ping sent; the next one is due an interval later, and the peer is
        // not dead yet — it still has the rest of the window to answer.
        keepalive.note_ping(start + TEST_PING);
        assert_eq!(
            keepalive.poll(start + TEST_PING),
            WsLiveness::Idle(TEST_PING)
        );
        assert_eq!(keepalive.poll(start + TEST_PING * 2), WsLiveness::Ping);

        keepalive.note_ping(start + TEST_PING * 2);
        assert_eq!(keepalive.poll(start + TEST_DEAD), WsLiveness::Dead);
    }

    /// A ping late in the window schedules its successor past the death
    /// deadline. Sleeping to *that* would let a dead peer outlive its own
    /// timeout, so the nap is capped at the deadline.
    #[test]
    fn the_nap_never_overshoots_the_death_deadline() {
        let start = Instant::now();
        let mut keepalive = WsKeepalive::with_timings(start, TEST_PING, TEST_DEAD);
        // Ping at 250ms: the next ping would be 350ms, but death is at 300ms.
        let late = start + Duration::from_millis(250);
        keepalive.note_ping(late);

        assert_eq!(
            keepalive.poll(late),
            WsLiveness::Idle(Duration::from_millis(50)),
            "wake at the death deadline, not at the next ping"
        );
    }

    /// A pong (or any other inbound message) rearms the whole policy: the
    /// death deadline moves and the ping schedule restarts from scratch.
    #[test]
    fn inbound_traffic_rearms_after_a_ping() {
        let start = Instant::now();
        let mut keepalive = WsKeepalive::with_timings(start, TEST_PING, TEST_DEAD);
        keepalive.note_ping(start + TEST_PING);

        let pong = start + TEST_PING + Duration::from_millis(10);
        keepalive.note_inbound(pong);

        assert_eq!(keepalive.poll(pong), WsLiveness::Idle(TEST_PING));
        assert_eq!(
            keepalive.poll(pong + TEST_DEAD - Duration::from_millis(1)),
            WsLiveness::Ping,
            "still alive: the deadline moved with the pong"
        );
        assert_eq!(keepalive.poll(pong + TEST_DEAD), WsLiveness::Dead);
    }

    /// The production timings match the QUIC lane's, and leave room for a
    /// dropped keepalive or two before condemning the connection.
    #[test]
    fn production_timings_mirror_the_quic_lane() {
        assert_eq!(WS_PING_INTERVAL, Duration::from_secs(10));
        assert_eq!(WS_LIVENESS_TIMEOUT, Duration::from_secs(30));
        assert!(
            WS_LIVENESS_TIMEOUT >= WS_PING_INTERVAL * 3,
            "one lost ping on a lossy link must not read as a dead peer"
        );
    }

    // ---- keepalive over a real socket ------------------------------------

    /// Accept one WebSocket connection and then go silent forever, holding
    /// the socket open without ever polling it.
    ///
    /// This is what the far end of a half-open TCP connection looks like from
    /// the client: the kernel took the bytes, nothing answers, and no FIN or
    /// RST is ever coming. It is the laptop-switched-networks shape, and it is
    /// precisely the case a plain `read` cannot survive.
    async fn spawn_silent_ws_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("accept");
            let ws = tokio_tungstenite::accept_async(tcp)
                .await
                .expect("ws upgrade");
            // Never polled: no pong is ever sent. Held so the socket stays
            // open — a closed socket would be the easy case.
            std::future::pending::<()>().await;
            drop(ws);
        });
        format!("ws://127.0.0.1:{port}")
    }

    /// Accept one WebSocket connection and keep draining it, which is what
    /// every RFC 6455 peer does — tungstenite answers pings automatically on
    /// the read path. The reference phux server's `WsListener` reads in
    /// exactly this shape, which is why the client-initiated ping works
    /// against servers that know nothing about this change.
    async fn spawn_responsive_ws_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("accept");
            let mut ws = tokio_tungstenite::accept_async(tcp)
                .await
                .expect("ws upgrade");
            while ws.next().await.is_some() {}
        });
        format!("ws://127.0.0.1:{port}")
    }

    async fn connect_halves(url: &str, keepalive: WsKeepalive) -> (WsReader, WsWriter) {
        let ws = dial(&WsDial {
            url: url.to_owned(),
            token: None,
            trust: CertTrust::SkipVerify,
            tls_server_name: None,
        })
        .await
        .expect("dial");
        let (tx, rx) = ws.split();
        (WsReader::with_keepalive(rx, keepalive), WsWriter { tx })
    }

    /// THE defect, pinned. Against a peer that has stopped answering,
    /// `recv_message` never returns — no EOF, no error, no timeout. Without
    /// the keepalive the attach loop parks here forever and the reconnect
    /// path is never even entered.
    #[tokio::test]
    async fn plain_recv_hangs_forever_on_a_stalled_peer() {
        let url = spawn_silent_ws_server().await;
        let (mut reader, _writer) = connect_halves(
            &url,
            WsKeepalive::with_timings(Instant::now(), TEST_PING, TEST_DEAD),
        )
        .await;

        let outcome = tokio::time::timeout(TEST_DEAD * 10, reader.recv_message()).await;
        assert!(
            outcome.is_err(),
            "plain recv_message must be the hanging read this fix exists to bound: {outcome:?}"
        );
    }

    /// The fix: the same stalled peer, read through the keepalive, is
    /// reported as `Stalled` within the liveness window instead of hanging.
    /// `Stalled` is what the client maps to a disconnect, so this is the edge
    /// that puts a network-switched laptop onto the reconnect path at all.
    ///
    /// The lower bound is measured from `opened`, the instant the policy
    /// takes as its own `last_inbound` — *not* from after the dial. The
    /// policy condemns the peer at `opened + TEST_DEAD` no matter how long
    /// the TCP connect and WebSocket handshake took, so timing the window
    /// from after the handshake silently subtracts the handshake from it.
    /// That is what made this test load-dependent (phux-5wxp.2): under a
    /// saturated pool it failed 10 times in 200 at 297.5ms-299.99975ms
    /// against the 300ms window, one of them short by 250 *nanoseconds*.
    /// Sharing the origin makes the bound load-independent — elapsed time
    /// only ever grows — while still failing loudly if the policy ever
    /// condemns a peer before its window is up.
    #[tokio::test]
    async fn keepalive_reports_a_stalled_peer_instead_of_hanging() {
        let url = spawn_silent_ws_server().await;
        let opened = Instant::now();
        let (mut reader, mut writer) = connect_halves(
            &url,
            WsKeepalive::with_timings(opened, TEST_PING, TEST_DEAD),
        )
        .await;

        let err =
            tokio::time::timeout(TEST_DEAD * 10, recv_message_alive(&mut reader, &mut writer))
                .await
                .expect("the keepalive must bound the read")
                .expect_err("a silent peer is not a clean close");

        assert!(matches!(err, DialError::Stalled(_)), "got {err:?}");
        assert!(
            err.to_string().starts_with("transport stalled: "),
            "got {err}"
        );
        assert!(
            opened.elapsed() >= TEST_DEAD,
            "must not condemn a peer before its window elapses: {:?}",
            opened.elapsed()
        );
    }

    /// No false positives: an ordinary RFC 6455 peer that is simply *quiet* —
    /// an attached session where nobody is typing and nothing is printing —
    /// answers the pings and stays up indefinitely. Run for many liveness
    /// windows so a policy that failed to rearm on a pong would be caught.
    #[tokio::test]
    async fn a_quiet_but_healthy_peer_is_never_declared_dead() {
        let url = spawn_responsive_ws_server().await;
        let (mut reader, mut writer) = connect_halves(
            &url,
            WsKeepalive::with_timings(Instant::now(), TEST_PING, TEST_DEAD),
        )
        .await;

        let outcome =
            tokio::time::timeout(TEST_DEAD * 8, recv_message_alive(&mut reader, &mut writer)).await;
        assert!(
            outcome.is_err(),
            "a quiet healthy connection stays open; got {outcome:?}"
        );
    }
}
