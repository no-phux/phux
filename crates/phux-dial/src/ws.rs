//! Native WebSocket dial transport: the TCP fallback to QUIC, one binary
//! message per length-prefixed phux frame, with the `Authorization: Bearer`
//! pairing token on the RFC 6455 upgrade.
//!
//! It also owns this lane's **liveness**. A laptop that switches networks
//! leaves the old TCP socket with no FIN or RST, so a read would park
//! forever. [`WsKeepalive`] applies RFC 6455 ping/pong at the QUIC lane's
//! 10s/30s cadence, and [`recv_message_alive`] is the read path that uses it.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{FutureExt, SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message};
use tokio_tungstenite::{WebSocketStream, client_async};

use crate::DialError;
use crate::tls::{CertTrust, TlsClientIdentity};

mod phase;
pub use phase::WsPhase;

/// How often an otherwise silent WebSocket sends a client-initiated ping.
/// Every RFC 6455 peer answers with a pong, so this needs no server support.
const WS_PING_INTERVAL: Duration = Duration::from_secs(10);

/// How long a WebSocket may go without *any* inbound message before the peer
/// is declared gone: three pings, so losing one is tolerated.
pub const WS_LIVENESS_TIMEOUT: Duration = WS_PING_INTERVAL.saturating_mul(3);

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

/// Ping/pong liveness policy for one WebSocket, as a clock-injected pure
/// state machine.
///
/// **Any** inbound message counts as proof of life, so a busy session never
/// pings.
#[derive(Debug, Clone, Copy)]
pub struct WsKeepalive {
    ping_interval: Duration,
    liveness_timeout: Duration,
    last_inbound: Instant,
    /// When the last ping went out; reset by inbound traffic.
    last_ping: Option<Instant>,
}

impl WsKeepalive {
    /// The production policy: 10s pings, [`WS_LIVENESS_TIMEOUT`].
    #[must_use]
    pub const fn new(now: Instant) -> Self {
        Self::with_timings(now, WS_PING_INTERVAL, WS_LIVENESS_TIMEOUT)
    }

    const fn with_timings(
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
    /// The TLS client identity to present on `wss://`. `None` reads it from
    /// `PHUX_WORKLOAD_CERT` / `PHUX_WORKLOAD_KEY`; `Some` is exactly this
    /// identity and never reads the environment.
    pub identity: Option<TlsClientIdentity>,
}

/// The established WebSocket stream type [`dial`] returns.
pub type Ws = WebSocketStream<ClientStream>;

/// The plain (`ws://`, loopback dev) or TLS (`wss://`) TCP stream underneath
/// the WebSocket.
pub type ClientStream = Box<dyn ByteStream>;

/// A bidirectional byte stream a WebSocket can run over.
pub trait ByteStream:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + Unpin + std::fmt::Debug
{
}

impl<T> ByteStream for T where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + Unpin + std::fmt::Debug
{
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
    dial_observed(d, |_| {}).await
}

/// [`dial`] with synchronous, payload-free establishment observations.
///
/// The observer must not block. A failed or cancelled operation leaves its
/// before-operation phase last; only a successful upgrade emits `Upgraded`.
/// Plain WS omits the TLS phases. Timeout ownership remains with the caller.
///
/// # Errors
///
/// Returns the same establishment errors as [`dial`].
pub async fn dial_observed(d: &WsDial, observe: impl Fn(WsPhase) + Sync) -> Result<Ws, DialError> {
    dial_inner(d, d.identity.as_ref(), &observe).await
}

/// [`dial`] with an explicit TLS identity, overriding [`WsDial::identity`];
/// never reads `PHUX_WORKLOAD_CERT` or `PHUX_WORKLOAD_KEY`.
///
/// # Errors
///
/// Returns the same establishment errors as [`dial`], plus explicit identity
/// file parse/read failures.
pub async fn dial_with_identity(d: &WsDial, identity: &TlsClientIdentity) -> Result<Ws, DialError> {
    dial_inner(d, Some(identity), &|_| {}).await
}

async fn dial_inner(
    d: &WsDial,
    identity: Option<&TlsClientIdentity>,
    observe: &(impl Fn(WsPhase) + Sync),
) -> Result<Ws, DialError> {
    let target = WsTarget::parse(&d.url)?;
    // URI authorities retain IPv6 brackets, but tuple-based DNS/TCP lookup
    // expects a bare address (the port is already a separate argument).
    let host = target.host.trim_matches(['[', ']']);
    // Resolve first so a name that does not resolve (MagicDNS down) is
    // classified as unreachable; the connect re-resolves from cache.
    observe(WsPhase::Resolve);
    if let Err(err) = tokio::net::lookup_host((host, target.port)).await {
        return Err(DialError::Unreachable(format!(
            "dial {}: name resolution failed: {err}",
            target.addr_label()
        )));
    }
    observe(WsPhase::TcpConnect);
    let tcp = TcpStream::connect((host, target.port))
        .await
        .map_err(|err| {
            let msg = format!("dial {}: {err}", target.addr_label());
            if crate::is_reachability_io(&err) {
                DialError::Unreachable(msg)
            } else {
                DialError::Connect(msg)
            }
        })?;
    // Nagle off: a keystroke is one small frame that Nagle would hold for the
    // peer's delayed ACK. A failure costs latency, not correctness.
    let _ = tcp.set_nodelay(true);
    let stream: ClientStream = if target.secure {
        Box::new(tls_connect(tcp, &target, d, identity, observe).await?)
    } else {
        Box::new(tcp)
    };

    observe(WsPhase::Upgrade);
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

    let (ws, _) = client_async(req, stream).await.map_err(ws_error)?;
    observe(WsPhase::Upgraded);
    Ok(ws)
}

async fn tls_connect(
    tcp: TcpStream,
    target: &WsTarget,
    dial: &WsDial,
    identity: Option<&TlsClientIdentity>,
    observe: &(impl Fn(WsPhase) + Sync),
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, DialError> {
    observe(WsPhase::TlsConfig);
    let (config, refusal) = crate::tls::client_config_reporting(&dial.trust, identity, None)?;
    let config = Arc::new(config);
    let connector = tokio_rustls::TlsConnector::from(config);
    let server_name = dial
        .tls_server_name
        .clone()
        .unwrap_or_else(|| target.server_name.clone());
    let server_name = rustls::pki_types::ServerName::try_from(server_name)
        .map_err(|err| DialError::Connect(format!("invalid TLS server name: {err}")))?;
    observe(WsPhase::TlsHandshake);
    connector.connect(server_name, tcp).await.map_err(|err| {
        crate::tls::authority_refusal(&refusal).unwrap_or_else(|| {
            DialError::Connect(format!("TLS handshake with {}: {err}", target.host))
        })
    })
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

    /// Decide native WebSocket trust and normalize its token before dialing.
    ///
    /// DNS and credential acquisition stay in the adapter. An explicit relay
    /// route is refused: this transport has no inner end-to-end session.
    ///
    /// # Errors
    ///
    /// Refuses plaintext, missing pins/tokens off loopback, malformed tokens,
    /// and relay routes on either loopback or routable endpoints.
    pub fn credentials(
        &self,
        trust: Option<CertTrust>,
        token: Option<String>,
        route: Option<&str>,
    ) -> Result<(CertTrust, Option<String>), crate::PlanRefusal> {
        if route.is_some() {
            return Err(crate::PlanRefusal::RelayRequiresQuic);
        }
        let loopback = self.is_loopback();
        if !loopback && !self.secure {
            return Err(crate::PlanRefusal::Plaintext);
        }
        let trust = CertTrust::for_endpoint(trust, loopback)?;
        if !loopback && token.as_deref().is_none_or(|token| token.trim().is_empty()) {
            return Err(crate::PlanRefusal::NoToken);
        }
        let token = token
            .map(|mut token| {
                let trimmed = token.trim();
                if trimmed.len() % 2 != 0 || !trimmed.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(crate::PlanRefusal::BadToken);
                }
                if trimmed.len() != token.len() {
                    let len = trimmed.len();
                    let leading = token.len() - token.trim_start().len();
                    token.truncate(leading + len);
                    drop(token.drain(..leading));
                }
                Ok(token)
            })
            .transpose()?;
        Ok((trust, token))
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
    /// Liveness state, kept on the reader so it survives the read future
    /// being dropped and rebuilt inside a `select!`.
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
    pub async fn send(&mut self, frame: &[u8]) -> Result<(), DialError> {
        self.tx
            .send(Message::Binary(frame.to_vec().into()))
            .await
            .map_err(ws_error)
    }

    /// Send an empty RFC 6455 ping.
    pub async fn send_ping(&mut self) -> Result<(), DialError> {
        self.tx
            .send(Message::Ping(Vec::new().into()))
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

    /// Take the next phux frame **only if one is already buffered**, without
    /// awaiting the socket, so the attach loop can coalesce a burst into one
    /// render pass.
    ///
    /// A single no-op-waker poll is safe because the caller always returns
    /// to an awaiting [`recv_message_alive`], which re-registers the waker.
    ///
    /// # Errors
    ///
    /// Propagates transport failures as [`DialError`]. A clean close reads
    /// as `Ok(None)`; the next awaiting read surfaces it.
    pub fn try_recv_message(&mut self) -> Result<Option<Vec<u8>>, DialError> {
        while let Some(next) = self.rx.next().now_or_never() {
            match self.observe(next)? {
                WsActivity::Message(data) => return Ok(Some(data)),
                WsActivity::Control => {}
                WsActivity::Closed => return Ok(None),
            }
        }
        Ok(None)
    }

    /// Await one inbound activity without hiding control frames.
    async fn recv_activity(&mut self) -> Result<WsActivity, DialError> {
        let next = self.rx.next().await;
        self.observe(next)
    }

    /// Classify one item from the stream, noting any inbound traffic as
    /// proof of life (tungstenite has already queued the pong for a ping).
    fn observe(
        &mut self,
        next: Option<Result<Message, TungsteniteError>>,
    ) -> Result<WsActivity, DialError> {
        match next {
            None | Some(Ok(Message::Close(_))) => Ok(WsActivity::Closed),
            Some(Err(err)) => Err(ws_error(err)),
            Some(Ok(message)) => {
                self.keepalive.note_inbound(Instant::now());
                Ok(match message {
                    Message::Binary(data) => WsActivity::Message(data.to_vec()),
                    _ => WsActivity::Control,
                })
            }
        }
    }
}

/// One observable inbound event from an established WebSocket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsActivity {
    /// One complete binary phux frame.
    Message(Vec<u8>),
    /// WebSocket-level activity with no phux payload.
    Control,
    /// A clean WebSocket close or end of stream.
    Closed,
}

/// Receive the next phux frame, pinging after 10s of silence and returning
/// [`DialError::Stalled`] after [`WS_LIVENESS_TIMEOUT`] with nothing back.
///
/// Takes both halves because liveness is asked on the sink and answered on
/// the stream.
///
/// # Cancel safety
///
/// Safe to drop and re-enter: all deadline state lives on `reader`, and the
/// ping is recorded before it is awaited, so cancellation cannot cause a
/// ping storm.
///
/// # Errors
///
/// [`DialError::Stalled`] when the peer stops answering; otherwise transport
/// failures.
pub async fn recv_message_alive(
    reader: &mut WsReader,
    writer: &mut WsWriter,
) -> Result<Option<Vec<u8>>, DialError> {
    loop {
        match recv_activity_alive(reader, writer).await? {
            WsActivity::Message(data) => return Ok(Some(data)),
            WsActivity::Control => {}
            WsActivity::Closed => return Ok(None),
        }
    }
}

/// [`recv_message_alive`] that also returns after a control frame, for
/// callers running a shorter probe deadline for which a pong is the answer.
///
/// # Cancel safety
///
/// As [`recv_message_alive`].
///
/// # Errors
///
/// As [`recv_message_alive`].
pub async fn recv_activity_alive(
    reader: &mut WsReader,
    writer: &mut WsWriter,
) -> Result<WsActivity, DialError> {
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
        match tokio::time::timeout(nap, reader.recv_activity()).await {
            Ok(result) => return result,
            // The nap elapsed: let `poll` decide between ping and dead.
            Err(_elapsed) => {}
        }
    }
}

#[cfg(test)]
mod tests;
