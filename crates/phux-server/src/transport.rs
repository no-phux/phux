//! Transport abstraction for the accept loop (`phux-486.4`).
//!
//! The server speaks one wire — length-prefixed phux frames (`docs/spec/proto.md`
//! §5) — over more than one transport. UDS is the default local transport; a
//! WebSocket transport lets browser consumers (the `phux-web` client) speak the
//! *identical* frames. We abstract at the **frame** level: each transport yields
//! complete encoded frames, so the per-client dispatch loop in [`crate::runtime`]
//! and the `FrameKind` codec are transport-agnostic and reused verbatim.
//!
//! Wire contract per transport:
//! * **UDS** — frames are length-prefixed on the byte stream, exactly as today.
//! * **WebSocket** — one binary message carries one complete encoded frame
//!   (the 4-byte length prefix is included, so the same `FrameKind::decode`
//!   path works on both ends). Text/ping/pong frames are ignored; a Close
//!   message is EOF.

#![allow(
    clippy::future_not_send,
    reason = "single-threaded tokio runtime per ADR-0003; the token-auth accept path captures !Send Rc state and never crosses threads"
)]

pub mod quic;
pub mod tls;
#[cfg(feature = "webtransport")]
pub mod webtransport;

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use phux_protocol::policy::{PeerIdentity, TransportType};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream, UnixListener};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request};
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

use phux_protocol::wire::frame::MAX_FRAME_LEN;

const LENGTH_PREFIX: usize = 4;
pub(crate) const WS_REJECTION_WARN_INTERVAL: Duration = Duration::from_secs(60);

/// Read side of a client connection: yields one complete encoded frame (length
/// prefix included) per call, or `None` at end-of-stream.
pub(crate) trait FrameReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>>;
}

/// Write side: writes one complete pre-encoded frame.
pub(crate) trait FrameWriter {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()>;

    /// Finish the server-to-client stream after its final frame.
    async fn close(&mut self) -> io::Result<()>;
}

/// A listener that accepts connections, each split into a frame reader + writer.
pub(crate) trait Incoming {
    type Reader: FrameReader + 'static;
    type Writer: FrameWriter + 'static;
    async fn accept(&self) -> io::Result<(Self::Reader, Self::Writer, PeerIdentity)>;

    /// Classify a non-fatal accept error for the shared accept loop's logging.
    ///
    /// The default preserves the loop's `ERROR` event for listener and resource
    /// failures. A listener may return a narrower disposition only for errors it
    /// created and can recognize without inspecting their display text.
    fn accept_error_disposition(&self, _error: &io::Error) -> AcceptErrorDisposition {
        AcceptErrorDisposition::Default
    }

    /// Whether an accept error means the whole incoming source is gone.
    ///
    /// Socket listeners keep serving after transient per-connection errors.
    /// A dial-out connector wraps one established QUIC connection, so an
    /// `accept_bi` error means the relay leg is lost and its supervisor must
    /// redial.
    fn accept_errors_are_fatal(&self) -> bool {
        false
    }
    /// Short transport label for logs (`"uds"` / `"ws"`).
    fn kind(&self) -> &'static str;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AcceptErrorDisposition {
    /// Preserve the shared accept loop's default `ERROR` event.
    Default,
    /// The listener recognized a privacy-safe peer-caused rejection. The loop
    /// always emits it at `DEBUG`; `warn_suppressed` adds the rate-limited
    /// default-visible summary and reports how many summaries were suppressed.
    PeerRejected {
        stage: &'static str,
        source_ip: IpAddr,
        warn_suppressed: Option<u64>,
    },
}

// ── Unix domain socket ───────────────────────────────────────────────────────

/// UDS read half: reassembles length-prefixed frames off the byte stream.
pub(crate) struct UdsReader {
    reader: OwnedReadHalf,
    header: [u8; LENGTH_PREFIX],
}

impl FrameReader for UdsReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        match self.reader.read_exact(&mut self.header).await {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(err) => return Err(err),
        }
        let body_len = u32::from_be_bytes(self.header);
        if !(1..=MAX_FRAME_LEN).contains(&body_len) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversized or empty frame",
            ));
        }
        let body_len = body_len as usize;
        let mut framed = BytesMut::with_capacity(LENGTH_PREFIX + body_len);
        framed.extend_from_slice(&self.header);
        framed.resize(LENGTH_PREFIX + body_len, 0);
        self.reader.read_exact(&mut framed[LENGTH_PREFIX..]).await?;
        Ok(Some(framed))
    }
}

/// UDS write half.
pub(crate) struct UdsWriter {
    writer: OwnedWriteHalf,
}

impl FrameWriter for UdsWriter {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.writer.write_all(frame).await
    }

    async fn close(&mut self) -> io::Result<()> {
        self.writer.shutdown().await
    }
}

/// UDS listener: a thin newtype around [`UnixListener`] so the `Incoming::accept`
/// impl doesn't shadow the inherent `UnixListener::accept`.
pub(crate) struct UdsListener(UnixListener);

impl UdsListener {
    pub(crate) const fn new(listener: UnixListener) -> Self {
        Self(listener)
    }

    /// The raw listening-socket descriptor, captured at startup for the
    /// graceful-upgrade handoff (ADR-0032): cleared of `FD_CLOEXEC` and
    /// inherited by the re-exec'd image so the socket stays bound.
    pub(crate) fn as_raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.0.as_raw_fd()
    }
}

impl Incoming for UdsListener {
    type Reader = UdsReader;
    type Writer = UdsWriter;

    async fn accept(&self) -> io::Result<(UdsReader, UdsWriter, PeerIdentity)> {
        let (stream, _addr) = self.0.accept().await?;
        let peer_identity = peer_identity_from_uds(&stream)?;
        let (reader, writer) = stream.into_split();
        Ok((
            UdsReader {
                reader,
                header: [0u8; LENGTH_PREFIX],
            },
            UdsWriter { writer },
            peer_identity,
        ))
    }

    fn kind(&self) -> &'static str {
        "uds"
    }
}

fn peer_identity_from_credentials(
    credentials: io::Result<(u32, Option<u32>)>,
) -> io::Result<PeerIdentity> {
    let (uid, pid) = credentials?;
    Ok(PeerIdentity {
        uid,
        pid,
        exe_path: None,
        mcp_host_key: None,
        transport: TransportType::UnixSocket,
        source_addr: None,
    })
}

/// Extract peer identity from a Unix domain socket.
#[cfg(target_os = "linux")]
fn peer_identity_from_uds(stream: &tokio::net::UnixStream) -> io::Result<PeerIdentity> {
    peer_identity_from_credentials(stream.peer_cred().map(|cred| {
        (
            cred.uid(),
            cred.pid().and_then(|pid| u32::try_from(pid).ok()),
        )
    }))
}

/// Extract peer identity from a Unix domain socket on Darwin.
#[cfg(target_os = "macos")]
fn peer_identity_from_uds(stream: &tokio::net::UnixStream) -> io::Result<PeerIdentity> {
    use std::os::fd::AsRawFd as _;

    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: both output pointers are valid for writes for the duration of
    // the call, and `stream` owns a live Unix-domain socket descriptor.
    let status = unsafe { libc::getpeereid(stream.as_raw_fd(), &raw mut uid, &raw mut gid) };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    peer_identity_from_credentials(Ok((uid, None)))
}

/// Reject UDS transports on targets without an authenticated peer credential API.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_identity_from_uds(_stream: &tokio::net::UnixStream) -> io::Result<PeerIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "authenticated Unix peer credentials are unavailable",
    ))
}

// ── WebSocket ────────────────────────────────────────────────────────────────

/// The byte stream under a WebSocket: plaintext TCP (local browser client,
/// loopback only) or TLS (remote consumer over `wss://`, ADR-0031). Both ends
/// are `Unpin`, so the `AsyncRead`/`AsyncWrite` forwarding below projects with
/// `Pin::new` and needs no `unsafe`.
pub(crate) enum ServerStream {
    /// Plaintext TCP — the loopback browser-client path.
    Plain(TcpStream),
    /// TLS-terminated — the authenticated remote-consumer path. Boxed because
    /// `TlsStream` is large and the `Plain` variant should stay cheap.
    Tls(Box<tokio_rustls::server::TlsStream<TcpStream>>),
}

impl tokio::io::AsyncRead for ServerStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for ServerStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_flush(cx),
            Self::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

type Ws = WebSocketStream<ServerStream>;

/// WebSocket listener: TCP + RFC 6455 upgrade, then one binary message per frame.
///
/// Optionally TLS-terminated and token-authenticated for remote consumers
/// (ADR-0031). When `tls` is set, each connection is wrapped in TLS before the
/// upgrade; when `tokens` is set, the upgrade request must carry a valid
/// `Authorization: Bearer <hex>` or the handshake is refused with HTTP 401.
/// Both unset is the historical loopback browser-client path.
pub(crate) struct WsListener {
    tcp: TcpListener,
    tls: Option<tokio_rustls::TlsAcceptor>,
    tokens: Option<std::sync::Arc<crate::auth::ReloadingTokenStore>>,
    rejection_warnings: Mutex<PeerRejectionWarnLimiter>,
}

impl WsListener {
    const fn from_parts(
        tcp: TcpListener,
        tls: Option<tokio_rustls::TlsAcceptor>,
        tokens: Option<std::sync::Arc<crate::auth::ReloadingTokenStore>>,
    ) -> Self {
        Self {
            tcp,
            tls,
            tokens,
            rejection_warnings: Mutex::new(PeerRejectionWarnLimiter::new()),
        }
    }

    /// Bind a plaintext, unauthenticated listener (loopback browser client).
    pub(crate) async fn bind(addr: SocketAddr) -> io::Result<Self> {
        Ok(Self::from_parts(TcpListener::bind(addr).await?, None, None))
    }

    /// Bind a TLS-terminated, token-authenticated listener for remote consumers.
    ///
    /// TLS is mandatory here: the bearer token is sent in the (TLS-protected)
    /// handshake, so there is no token-over-plaintext path. ADR-0031's
    /// no-plaintext-remote invariant is enforced by this constructor being the
    /// only way to attach a token store.
    pub(crate) async fn bind_secure(
        addr: SocketAddr,
        tls: tokio_rustls::TlsAcceptor,
        tokens: std::sync::Arc<crate::auth::ReloadingTokenStore>,
    ) -> io::Result<Self> {
        Ok(Self::from_parts(
            TcpListener::bind(addr).await?,
            Some(tls),
            Some(tokens),
        ))
    }

    pub(crate) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.tcp.local_addr()
    }
}

/// WebSocket read half: each binary message is one complete encoded frame.
pub(crate) struct WsReader {
    rx: futures_util::stream::SplitStream<Ws>,
}

impl FrameReader for WsReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        loop {
            match self.rx.next().await {
                None | Some(Ok(Message::Close(_))) => return Ok(None),
                Some(Ok(Message::Binary(data))) => {
                    let len = data.len();
                    if len < LENGTH_PREFIX || len > LENGTH_PREFIX + MAX_FRAME_LEN as usize {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "websocket frame out of bounds",
                        ));
                    }
                    return Ok(Some(BytesMut::from(&data[..])));
                }
                Some(Err(err)) => return Err(io::Error::other(err)),
                // Ignore text / ping / pong / raw — the wire is binary frames only.
                Some(Ok(_)) => {}
            }
        }
    }
}

/// WebSocket write half.
pub(crate) struct WsWriter {
    tx: futures_util::stream::SplitSink<Ws, Message>,
}

impl FrameWriter for WsWriter {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.tx
            .send(Message::Binary(frame.to_vec()))
            .await
            .map_err(io::Error::other)
    }

    async fn close(&mut self) -> io::Result<()> {
        self.tx.close().await.map_err(io::Error::other)
    }
}

impl Incoming for WsListener {
    type Reader = WsReader;
    type Writer = WsWriter;

    async fn accept(&self) -> io::Result<(WsReader, WsWriter, PeerIdentity)> {
        let (tcp, peer) = self.tcp.accept().await?;
        // The remote ephemeral port is neither useful for pairing diagnosis nor
        // stable enough to be an identity field. Retain only the source IP.
        let source_ip = peer.ip();

        // TLS handshake first (if configured), so the bearer token in the
        // upgrade request is already encrypted when we read it. The underlying
        // TLS error is deliberately discarded: certificate and handshake
        // details are not part of the default-log privacy surface.
        let stream = match &self.tls {
            Some(acceptor) => ServerStream::Tls(Box::new(
                acceptor
                    .accept(tcp)
                    .await
                    .map_err(|_| ws_accept_error(WsAcceptStage::TlsHandshake, source_ip))?,
            )),
            None => ServerStream::Plain(tcp),
        };

        // WebSocket upgrade. With a token store, validate the
        // `Authorization: Bearer` header during the handshake and refuse with
        // HTTP 401 before any phux frame is read; the matched device's
        // (non-reversible) id is captured for the peer identity. Without one,
        // this is the historical anonymous browser-client path.
        let (ws, device_id) = match &self.tokens {
            Some(store) => {
                let store = store.clone();
                let captured: std::rc::Rc<std::cell::RefCell<Option<String>>> =
                    std::rc::Rc::new(std::cell::RefCell::new(None));
                let sink = captured.clone();
                let ws = tokio_tungstenite::accept_hdr_async(stream, move |req: &Request, resp| {
                    authorize_request(req, &store).map_or_else(
                        || Err(unauthorized_response()),
                        |id| {
                            *sink.borrow_mut() = Some(id);
                            Ok(resp)
                        },
                    )
                })
                .await
                .map_err(|error| classify_ws_upgrade_error(&error, source_ip))?;
                let id = captured.borrow_mut().take();
                (ws, id)
            }
            None => (
                tokio_tungstenite::accept_async(stream)
                    .await
                    .map_err(|_| ws_accept_error(WsAcceptStage::Upgrade, source_ip))?,
                None,
            ),
        };

        // An authenticated remote consumer is a first-class peer: its
        // device id rides `mcp_host_key` (the existing attestation slot), so
        // policy and audit see a non-anonymous identity rather than the
        // `uid: 0` stamp the plaintext browser path carries. Log the explicitly
        // privacy-safe fields before the identity moves into server state.
        if let Some(device_pseudonym) = device_id.as_deref() {
            tracing::info!(
                transport = "ws",
                %source_ip,
                device_pseudonym,
                "paired WebSocket consumer admitted"
            );
        }
        let peer_identity = PeerIdentity {
            uid: 0,
            pid: None,
            exe_path: None,
            mcp_host_key: device_id,
            transport: TransportType::WebSocket,
            source_addr: Some(source_ip),
        };

        let (tx, rx) = ws.split();
        Ok((WsReader { rx }, WsWriter { tx }, peer_identity))
    }

    fn accept_error_disposition(&self, error: &io::Error) -> AcceptErrorDisposition {
        let Some(rejection) = error
            .get_ref()
            .and_then(|source| source.downcast_ref::<WsPeerRejection>())
        else {
            return AcceptErrorDisposition::Default;
        };

        let decision = {
            let mut limiter = match self.rejection_warnings.lock() {
                Ok(limiter) => limiter,
                Err(poisoned) => poisoned.into_inner(),
            };
            limiter.observe(Instant::now())
        };
        let warn_suppressed = match decision {
            PeerRejectionWarnDecision::Suppress => None,
            PeerRejectionWarnDecision::Emit { suppressed } => Some(suppressed),
        };
        AcceptErrorDisposition::PeerRejected {
            stage: rejection.stage.as_str(),
            source_ip: rejection.source_ip,
            warn_suppressed,
        }
    }

    fn kind(&self) -> &'static str {
        "ws"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WsAcceptStage {
    TlsHandshake,
    PairingAuthentication,
    Upgrade,
}

impl WsAcceptStage {
    const fn as_str(self) -> &'static str {
        match self {
            Self::TlsHandshake => "tls_handshake",
            Self::PairingAuthentication => "pairing_authentication",
            Self::Upgrade => "websocket_upgrade",
        }
    }
}

/// A typed, privacy-safe peer rejection created by [`WsListener`].
///
/// It intentionally carries no source error: TLS and HTTP/WebSocket errors can
/// contain request, URI, header, certificate, or token material that must not
/// reach default logs. The shared accept loop recognizes this concrete type
/// rather than parsing display strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WsPeerRejection {
    stage: WsAcceptStage,
    source_ip: IpAddr,
}

impl std::fmt::Display for WsPeerRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let diagnosis = match self.stage {
            WsAcceptStage::TlsHandshake => "WebSocket TLS handshake failed",
            WsAcceptStage::PairingAuthentication => "WebSocket pairing authentication rejected",
            WsAcceptStage::Upgrade => "WebSocket upgrade failed",
        };
        write!(formatter, "{diagnosis} (source_ip={})", self.source_ip)
    }
}

impl std::error::Error for WsPeerRejection {}

fn ws_accept_error(stage: WsAcceptStage, source_ip: IpAddr) -> io::Error {
    io::Error::other(WsPeerRejection { stage, source_ip })
}

/// Authentication is the one HTTP response generated by our callback. All
/// other failures belong to the WebSocket upgrade stage. Neither branch keeps
/// or formats the underlying tungstenite error.
fn classify_ws_upgrade_error(error: &WebSocketError, source_ip: IpAddr) -> io::Error {
    ws_accept_error(classify_ws_upgrade_stage(error), source_ip)
}

fn classify_ws_upgrade_stage(error: &WebSocketError) -> WsAcceptStage {
    match error {
        WebSocketError::Http(response)
            if response.status()
                == tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED =>
        {
            WsAcceptStage::PairingAuthentication
        }
        _ => WsAcceptStage::Upgrade,
    }
}

#[derive(Debug)]
struct PeerRejectionWarnLimiter {
    last_warning: Option<Instant>,
    suppressed: u64,
}

impl PeerRejectionWarnLimiter {
    const fn new() -> Self {
        Self {
            last_warning: None,
            suppressed: 0,
        }
    }

    /// Pure state transition over an injected monotonic timestamp. The first
    /// rejection warns immediately; later warnings occur at most once per
    /// interval. One listener-wide counter keeps memory bounded independently
    /// of how many source addresses connect.
    fn observe(&mut self, now: Instant) -> PeerRejectionWarnDecision {
        let should_warn = self
            .last_warning
            .is_none_or(|last| now.saturating_duration_since(last) >= WS_REJECTION_WARN_INTERVAL);
        if should_warn {
            self.last_warning = Some(now);
            let suppressed = std::mem::take(&mut self.suppressed);
            PeerRejectionWarnDecision::Emit { suppressed }
        } else {
            self.suppressed = self.suppressed.saturating_add(1);
            PeerRejectionWarnDecision::Suppress
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeerRejectionWarnDecision {
    Suppress,
    Emit { suppressed: u64 },
}

/// Extract and verify the `Authorization: Bearer <hex>` pairing token from a
/// WebSocket upgrade request. Returns a non-reversible device id (a short
/// SHA-256 prefix of the *presented* token) on success, `None` on a missing,
/// malformed, or unrecognized token.
fn authorize_request(req: &Request, store: &crate::auth::ReloadingTokenStore) -> Option<String> {
    let header = req.headers().get("authorization")?.to_str().ok()?;
    let token_hex = header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))?
        .trim();
    let token = hex::decode(token_hex).ok()?;
    if !store.verify(&token) {
        return None;
    }
    // Device id is derived from the presented token (not from which stored
    // token matched), so deriving it never branches on the constant-time
    // comparison and never logs the secret itself.
    let digest = Sha256::digest(&token);
    Some(hex::encode(&digest[..8]))
}

/// The HTTP 401 the handshake returns when the pairing token is absent or
/// invalid. The body is deliberately generic — it does not distinguish
/// "missing" from "wrong" so it leaks nothing about the token namespace.
fn unauthorized_response() -> ErrorResponse {
    use tokio_tungstenite::tungstenite::http::StatusCode;
    let mut resp = ErrorResponse::new(Some("missing or invalid pairing token".to_owned()));
    *resp.status_mut() = StatusCode::UNAUTHORIZED;
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::sync::Arc;

    use tokio::net::TcpStream;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    const TEST_TOKEN: [u8; crate::auth::TOKEN_LEN] = [0x11; crate::auth::TOKEN_LEN];

    /// A token-gated listener bound to an ephemeral loopback port, with one
    /// known token. TLS is off so the test exercises the token handshake and
    /// frame path without the TLS machinery (covered in `tls`'s own tests).
    ///
    /// The `NamedTempFile` is returned, not dropped: the store re-reads it on
    /// every connection (phux-0d92), so deleting it would revoke every token.
    async fn token_listener() -> (WsListener, SocketAddr, String, tempfile::NamedTempFile) {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let token_hex = hex::encode(TEST_TOKEN);
        writeln!(file, "{token_hex}").unwrap();
        let store = crate::auth::ReloadingTokenStore::load(file.path().to_path_buf()).unwrap();

        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let listener = WsListener::from_parts(tcp, None, Some(Arc::new(store)));
        (listener, addr, token_hex, file)
    }

    /// A `ws://` client upgrade request carrying `Authorization: Bearer <hex>`.
    fn bearer_request(addr: SocketAddr, token_hex: &str) -> Request {
        let mut req = format!("ws://{addr}/").into_client_request().unwrap();
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {token_hex}").parse().unwrap(),
        );
        req
    }

    #[tokio::test]
    async fn valid_token_upgrades_and_round_trips_a_frame() {
        let (listener, addr, token_hex, _tokens) = token_listener().await;

        // One complete framed message: 4-byte length prefix (body = 3) + body.
        let frame: Vec<u8> = vec![0, 0, 0, 3, 0xde, 0xad, 0xbe];

        let server = async {
            let (mut reader, _writer, peer) = listener.accept().await.unwrap();
            let got = reader.read_frame().await.unwrap();
            (got, peer)
        };
        let client = async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            let (mut ws, _resp) =
                tokio_tungstenite::client_async(bearer_request(addr, &token_hex), tcp)
                    .await
                    .expect("valid token must upgrade");
            ws.send(Message::Binary(frame.clone())).await.unwrap();
            // Hold the connection open until the server has read.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };

        let ((got, peer), ()) = tokio::join!(server, client);
        assert_eq!(got.unwrap().as_ref(), frame.as_slice(), "frame round-trips");
        let expected_device = hex::encode(&Sha256::digest(TEST_TOKEN)[..8]);
        assert_eq!(peer.transport, TransportType::WebSocket);
        assert_eq!(peer.source_addr, Some(addr.ip()));
        assert_eq!(peer.mcp_host_key.as_deref(), Some(expected_device.as_str()));
        assert_eq!(peer.uid, 0);
        assert_eq!(peer.pid, None);
        assert_eq!(peer.exe_path, None);
    }

    async fn refused_handshake(
        listener: &WsListener,
        addr: SocketAddr,
        request: Request,
    ) -> (String, WebSocketError) {
        let server = listener.accept();
        let client = async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            tokio_tungstenite::client_async(request, tcp).await
        };
        let (server_result, client_result) = tokio::join!(server, client);
        let server_error = match server_result {
            Err(error) => error.to_string(),
            Ok(_) => panic!("server unexpectedly admitted rejected handshake"),
        };
        let Err(client_error) = client_result else {
            panic!("client unexpectedly completed rejected handshake");
        };
        (server_error, client_error)
    }

    fn assert_generic_unauthorized(error: WebSocketError) {
        let WebSocketError::Http(response) = error else {
            panic!("expected HTTP rejection");
        };
        assert_eq!(
            response.status(),
            tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            response.body().as_deref(),
            Some(b"missing or invalid pairing token".as_slice())
        );
    }

    #[tokio::test]
    async fn invalid_malformed_and_missing_tokens_are_identical_safe_rejections() {
        let (listener, addr, _token_hex, _tokens) = token_listener().await;
        let wrong = hex::encode([0x22u8; crate::auth::TOKEN_LEN]);

        let (invalid_error, invalid_response) =
            refused_handshake(&listener, addr, bearer_request(addr, &wrong)).await;
        let (malformed_error, malformed_response) =
            refused_handshake(&listener, addr, bearer_request(addr, "not-hex")).await;
        let missing_request = format!("ws://{addr}/").into_client_request().unwrap();
        let (missing_error, missing_response) =
            refused_handshake(&listener, addr, missing_request).await;

        assert_generic_unauthorized(invalid_response);
        assert_generic_unauthorized(malformed_response);
        assert_generic_unauthorized(missing_response);
        assert_eq!(invalid_error, malformed_error);
        assert_eq!(invalid_error, missing_error);
        assert_eq!(
            invalid_error,
            format!(
                "WebSocket pairing authentication rejected (source_ip={})",
                addr.ip()
            )
        );
        assert!(!invalid_error.contains(&wrong));
        assert!(!invalid_error.contains("not-hex"));
        assert!(!invalid_error.to_ascii_lowercase().contains("authorization"));
        assert!(!invalid_error.to_ascii_lowercase().contains("bearer"));
        assert!(!invalid_error.contains(&addr.port().to_string()));
    }

    /// phux-0d92: ADR-0081 promises `phux pair` is a pure credential operation
    /// needing no restart. The listener is bound before the token exists, so
    /// this is the promise as a test.
    #[tokio::test]
    async fn a_token_minted_after_bind_upgrades_without_a_restart() {
        let (listener, addr, _token_hex, mut tokens) = token_listener().await;
        let paired = hex::encode([0x33u8; crate::auth::TOKEN_LEN]);

        // Before pairing, the device is a stranger.
        let refused = async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            tokio_tungstenite::client_async(bearer_request(addr, &paired), tcp).await
        };
        let (server_res, client_res) = tokio::join!(listener.accept(), refused);
        assert!(server_res.is_err(), "unpaired device is refused");
        assert!(client_res.is_err());

        // `phux pair` appends the token to the store the server is already
        // serving from. Nothing restarts.
        writeln!(tokens, "{paired}").unwrap();
        tokens.flush().unwrap();

        let accepted = async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            let (ws, _resp) = tokio_tungstenite::client_async(bearer_request(addr, &paired), tcp)
                .await
                .expect("a freshly paired device upgrades against the running listener");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            drop(ws);
        };
        let (server_res, ()) = tokio::join!(listener.accept(), accepted);
        assert!(
            server_res.is_ok(),
            "the newly minted token is live without a restart"
        );
    }

    /// The other half of phux-0d92: deleting a line revokes the device at the
    /// next connection attempt, which today also needs a restart.
    #[tokio::test]
    async fn a_revoked_token_is_refused_without_a_restart() {
        let (listener, addr, token_hex, tokens) = token_listener().await;

        std::fs::write(tokens.path(), "# every device revoked\n").unwrap();

        let refused = async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            tokio_tungstenite::client_async(bearer_request(addr, &token_hex), tcp).await
        };
        let (server_res, client_res) = tokio::join!(listener.accept(), refused);
        assert!(server_res.is_err(), "a revoked token no longer upgrades");
        assert!(client_res.is_err());
    }

    #[test]
    fn websocket_accept_errors_are_typed_and_classified_without_display_parsing() {
        let source_ip = "192.0.2.41".parse().unwrap();
        let tls_error = ws_accept_error(WsAcceptStage::TlsHandshake, source_ip);
        assert_eq!(
            tls_error.to_string(),
            "WebSocket TLS handshake failed (source_ip=192.0.2.41)"
        );
        let typed = tls_error
            .get_ref()
            .and_then(|source| source.downcast_ref::<WsPeerRejection>())
            .expect("listener errors retain the safe concrete type");
        assert_eq!(typed.stage, WsAcceptStage::TlsHandshake);
        assert_eq!(typed.source_ip, source_ip);

        let auth_error = WebSocketError::Http(
            tokio_tungstenite::tungstenite::http::Response::builder()
                .status(tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED)
                .body(None::<Vec<u8>>)
                .unwrap(),
        );
        assert_eq!(
            classify_ws_upgrade_stage(&auth_error),
            WsAcceptStage::PairingAuthentication
        );

        let unsafe_underlying = WebSocketError::Protocol(
            tokio_tungstenite::tungstenite::error::ProtocolError::InvalidHeader(
                "authorization".parse().unwrap(),
            ),
        );
        assert_eq!(
            classify_ws_upgrade_stage(&unsafe_underlying),
            WsAcceptStage::Upgrade
        );
        let safe_upgrade = classify_ws_upgrade_error(&unsafe_underlying, source_ip).to_string();
        assert_eq!(
            safe_upgrade,
            "WebSocket upgrade failed (source_ip=192.0.2.41)"
        );
        assert!(!safe_upgrade.contains("authorization"));
    }

    #[test]
    fn peer_rejection_warning_limiter_is_global_bounded_and_deterministic() {
        let start = Instant::now();
        let mut limiter = PeerRejectionWarnLimiter::new();

        assert_eq!(
            limiter.observe(start),
            PeerRejectionWarnDecision::Emit { suppressed: 0 }
        );
        assert_eq!(
            limiter.observe(
                start + WS_REJECTION_WARN_INTERVAL.saturating_sub(Duration::from_nanos(1)),
            ),
            PeerRejectionWarnDecision::Suppress
        );
        assert_eq!(
            limiter.observe(start + WS_REJECTION_WARN_INTERVAL),
            PeerRejectionWarnDecision::Emit { suppressed: 1 }
        );
        assert_eq!(
            limiter.observe(start + WS_REJECTION_WARN_INTERVAL),
            PeerRejectionWarnDecision::Suppress
        );
        assert_eq!(
            limiter.observe(start + WS_REJECTION_WARN_INTERVAL * 2),
            PeerRejectionWarnDecision::Emit { suppressed: 1 }
        );

        limiter.suppressed = u64::MAX;
        assert_eq!(
            limiter.observe(start + WS_REJECTION_WARN_INTERVAL * 2),
            PeerRejectionWarnDecision::Suppress
        );
        assert_eq!(limiter.suppressed, u64::MAX);
    }

    #[test]
    fn uds_credential_failure_is_never_root_fallback() {
        let error = peer_identity_from_credentials(Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "simulated peer credential failure",
        )))
        .expect_err("missing authenticated credentials must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
}
