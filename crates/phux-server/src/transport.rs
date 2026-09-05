//! Transport abstraction for the accept loop (`phux-486.4`).
//!
//! The server speaks one wire — length-prefixed phux frames (`docs/spec/proto.md`
//! §5) — over more than one transport. UDS is the default local transport; a
//! WebSocket transport lets browser consumers (the `phux-web` client) speak the
//! *identical* frames. We abstract at the **frame** level: each transport yields
//! complete encoded frames, so the per-client dispatch loop in [`crate::runtime`]
//! and the `FrameKind` codec are transport-agnostic and reused verbatim.
//!
//! The §5 rule itself is not restated here: every reader below defers to
//! [`phux_protocol::wire::framing`], which owns it.
//!
//! Wire contract per transport:
//! * **UDS** — frames are length-prefixed on the byte stream, exactly as today.
//! * **WebSocket** — one binary message carries one complete encoded frame
//!   (the 4-byte length prefix is included, so the same `FrameKind::decode`
//!   path works on both ends). "Exactly one" is enforced: a message whose size
//!   disagrees with the `length` it declares is malformed, not a batch, since
//!   §5 defines no second framing layer. Text/ping/pong frames are ignored; a
//!   Close message is EOF.

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
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream, UnixListener};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request};
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

use phux_protocol::wire::framing::{self, LENGTH_PREFIX_LEN as LENGTH_PREFIX};
pub(crate) const WS_REJECTION_WARN_INTERVAL: Duration = Duration::from_secs(60);

/// How long a peer has to finish the TLS handshake and the WebSocket upgrade
/// before the connection is refused.
///
/// The accept loop in [`crate::runtime::client`] awaits `accept()` to
/// completion before it can accept anyone else, so an un-timed handshake makes
/// a single stalled peer a permanent denial of service on the whole listener:
/// the kernel keeps completing TCP handshakes, so later clients connect and
/// then wait forever for bytes userspace will never send. A peer that connects
/// and simply never speaks costs nothing to create, which makes this reachable
/// by accident (a sleeping phone whose RST never arrives, a stray port probe)
/// as well as on purpose.
///
/// This mirrors `phux-relay`'s `PREAMBLE_DEADLINE`: a legitimate client starts
/// its handshake immediately, so the bound only fires on stalled peers.
pub(crate) const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// Read side of a client connection: yields one complete encoded frame (length
/// prefix included) per call, or `None` at end-of-stream.
pub(crate) trait FrameReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>>;
}

/// Write side: writes one complete pre-encoded frame.
pub(crate) trait FrameWriter {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()>;

    /// Write several already-encoded frames that sit back-to-back in `batch`,
    /// where `ends` holds each frame's exclusive end offset (so frame `i` is
    /// `batch[ends[i - 1]..ends[i]]`, with an implicit `0` before the first).
    ///
    /// The default is one [`Self::write_frame`] per frame, which is what a
    /// **message-oriented** transport requires: there, a frame boundary IS a
    /// transport message boundary and merging two frames into one write would
    /// corrupt the stream. WebSocket is the only such transport phux speaks —
    /// its writer hands each frame to `Message::Binary`.
    ///
    /// A **byte-stream** transport has no such constraint: its frames are
    /// self-delimiting via the length prefix `FrameKind::encode` already
    /// wrote, so it overrides this with a single write of the whole batch.
    /// UDS, QUIC, and WebTransport are all in this class — the QUIC and
    /// WebTransport writers each own one reliable ordered *stream*, not a
    /// datagram flow, and their readers reassemble by length prefix exactly
    /// as `UdsReader` does. See `UdsWriter`, `QuicWriter`, `WtWriter`.
    async fn write_frames(&mut self, batch: &[u8], ends: &[usize]) -> io::Result<()> {
        let mut start = 0;
        for &end in ends {
            self.write_frame(&batch[start..end]).await?;
            start = end;
        }
        Ok(())
    }

    /// Push whatever [`Self::write_frames`] buffered out to the peer.
    ///
    /// The writer task calls this once per drain of the outbound mailbox, not
    /// once per frame. That is what lets a message-oriented transport keep the
    /// one-message-per-frame framing `write_frames` requires and still pay a
    /// single `write(2)` for the whole burst: `WsWriter::write_frame` only
    /// feeds tungstenite's buffer, and this is where it leaves. Stream
    /// transports write straight through, so the default is a no-op and their
    /// per-frame cost is unchanged.
    async fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Finish the server-to-client stream after its final frame.
    async fn close(&mut self) -> io::Result<()>;
}

/// A listener that accepts connections, each split into a frame reader + writer.
pub(crate) trait Incoming {
    type Reader: FrameReader + 'static;
    type Writer: FrameWriter + 'static;
    async fn accept(
        &self,
    ) -> io::Result<(Self::Reader, Self::Writer, crate::auth::ConnectionIdentity)>;

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
        let mut framed = framing::frame_buffer(self.header)?;
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

    /// One `write_all` for the whole batch. UDS is a byte stream and every
    /// frame in `batch` already carries its own length prefix, so the bytes
    /// on the wire are identical to writing them one at a time — the client's
    /// reassembler cannot tell the difference. What changes is the syscall
    /// count: a burst that queued N frames behind a busy writer now costs one
    /// `write(2)` instead of N. `ends` is unused for exactly that reason.
    async fn write_frames(&mut self, batch: &[u8], _ends: &[usize]) -> io::Result<()> {
        self.writer.write_all(batch).await
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

    async fn accept(&self) -> io::Result<(UdsReader, UdsWriter, crate::auth::ConnectionIdentity)> {
        let (stream, _addr) = self.0.accept().await?;
        let peer_identity = peer_identity_from_uds(&stream)?;
        let (reader, writer) = stream.into_split();
        Ok((
            UdsReader {
                reader,
                header: [0u8; LENGTH_PREFIX],
            },
            UdsWriter { writer },
            peer_identity.into(),
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
                    // One binary message carries exactly one frame — SPEC §5
                    // has no second framing layer, so a message that declares
                    // a length disagreeing with its own size is malformed,
                    // not a batch. Checking it here rather than tolerating a
                    // non-empty decode tail keeps the WebSocket path as strict
                    // as the stream transports.
                    framing::check_frame(&data)?;
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
    /// Queue one frame as one binary message **without** flushing.
    ///
    /// `SinkExt::send` is `feed` + `flush`, and flushing per frame is what
    /// made this transport the slow one (phux-l96p.10): a `seq 1 300000`
    /// burst is tens of thousands of 4 KiB frames, and each one cost a
    /// separate `write(2)` of a partial TCP segment. `feed` lets tungstenite
    /// accumulate them in its 128 KiB write buffer; the writer task flushes
    /// once per drain of the outbound mailbox, so a burst leaves as full
    /// segments and an idle connection still flushes on its single frame.
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.tx
            .feed(Message::Binary(frame.to_vec()))
            .await
            .map_err(io::Error::other)
    }

    async fn flush(&mut self) -> io::Result<()> {
        SinkExt::flush(&mut self.tx).await.map_err(io::Error::other)
    }

    async fn close(&mut self) -> io::Result<()> {
        self.tx.close().await.map_err(io::Error::other)
    }
}

impl Incoming for WsListener {
    type Reader = WsReader;
    type Writer = WsWriter;

    async fn accept(&self) -> io::Result<(WsReader, WsWriter, crate::auth::ConnectionIdentity)> {
        let (tcp, peer) = self.tcp.accept().await?;
        // The remote ephemeral port is neither useful for pairing diagnosis nor
        // stable enough to be an identity field. Retain only the source IP.
        let source_ip = peer.ip();

        // Nagle off (phux-l96p.10). A terminal is a latency wire: the server
        // answers a keystroke with a short `TERMINAL_OUTPUT`, which Nagle
        // holds until the peer's delayed ACK returns, adding tens of
        // milliseconds to every echo. UDS has no such algorithm and QUIC does
        // not implement one, which is why only this transport showed a 33 ms
        // echo p99 against 0.7 ms on the other two. Failure is not fatal —
        // the connection still works, only slower — so it is logged, not
        // propagated.
        if let Err(err) = tcp.set_nodelay(true) {
            tracing::debug!(error = %err, "could not disable Nagle on accepted WebSocket TCP stream");
        }

        // TLS handshake first (if configured), so the bearer token in the
        // upgrade request is already encrypted when we read it. The underlying
        // TLS error is deliberately discarded: certificate and handshake
        // details are not part of the default-log privacy surface.
        // Bounded by `HANDSHAKE_DEADLINE`: a peer that stalls mid-handshake
        // must not hold the accept loop, and so the whole listener, forever.
        // A timeout is reported as the same stage as a handshake failure —
        // both are "this peer never completed TLS", and neither reveals
        // anything about the certificate or the peer beyond its source IP.
        let stream = match &self.tls {
            Some(acceptor) => ServerStream::Tls(Box::new(
                tokio::time::timeout(HANDSHAKE_DEADLINE, acceptor.accept(tcp))
                    .await
                    .map_err(|_| ws_accept_error(WsAcceptStage::TlsHandshake, source_ip))?
                    .map_err(|_| ws_accept_error(WsAcceptStage::TlsHandshake, source_ip))?,
            )),
            None => ServerStream::Plain(tcp),
        };

        // WebSocket upgrade. With a token store, validate the
        // `Authorization: Bearer` header during the handshake and refuse with
        // HTTP 401 before any phux frame is read; the matched device's
        // (non-reversible) id is captured for the peer identity. Without one,
        // this is the historical anonymous browser-client path.
        let (ws, credential) = match &self.tokens {
            Some(store) => {
                let store = store.clone();
                let captured: std::rc::Rc<
                    std::cell::RefCell<Option<crate::auth::AuthenticatedCredential>>,
                > = std::rc::Rc::new(std::cell::RefCell::new(None));
                let sink = captured.clone();
                let ws = tokio::time::timeout(
                    HANDSHAKE_DEADLINE,
                    tokio_tungstenite::accept_hdr_async(stream, move |req: &Request, resp| {
                        authorize_request(req, &store).map_or_else(
                            || Err(unauthorized_response()),
                            |credential| {
                                *sink.borrow_mut() = Some(credential);
                                Ok(resp)
                            },
                        )
                    }),
                )
                .await
                .map_err(|_| ws_accept_error(WsAcceptStage::Upgrade, source_ip))?
                .map_err(|error| classify_ws_upgrade_error(&error, source_ip))?;
                let id = captured.borrow_mut().take();
                (ws, id)
            }
            None => (
                tokio::time::timeout(HANDSHAKE_DEADLINE, tokio_tungstenite::accept_async(stream))
                    .await
                    .map_err(|_| ws_accept_error(WsAcceptStage::Upgrade, source_ip))?
                    .map_err(|_| ws_accept_error(WsAcceptStage::Upgrade, source_ip))?,
                None,
            ),
        };

        // An authenticated remote consumer is a first-class peer: its
        // device id rides `mcp_host_key` (the existing attestation slot), so
        // policy and audit see a non-anonymous identity rather than the
        // `uid: 0` stamp the plaintext browser path carries. Log the explicitly
        // privacy-safe fields before the identity moves into server state.
        if let Some(credential) = credential.as_ref() {
            tracing::info!(
                transport = "ws",
                %source_ip,
                credential_id = %credential.id,
                "paired WebSocket consumer admitted"
            );
        }
        let peer_identity = PeerIdentity {
            uid: 0,
            pid: None,
            exe_path: None,
            mcp_host_key: credential.as_ref().map(|credential| credential.id.clone()),
            transport: TransportType::WebSocket,
            source_addr: Some(source_ip),
        };

        let (tx, rx) = ws.split();
        Ok((
            WsReader { rx },
            WsWriter { tx },
            crate::auth::ConnectionIdentity {
                peer: peer_identity,
                credential,
            },
        ))
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
/// WebSocket upgrade request. Returns the stable credential id on success,
/// `None` on a missing, malformed, or unrecognized token.
fn authorize_request(
    req: &Request,
    store: &crate::auth::ReloadingTokenStore,
) -> Option<crate::auth::AuthenticatedCredential> {
    let header = req.headers().get("authorization")?.to_str().ok()?;
    let token_hex = header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))?
        .trim();
    let token = hex::decode(token_hex).ok()?;
    store.authenticate(&token)
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
    use std::sync::Arc;

    use phux_protocol::PROTOCOL_VERSION;
    use phux_protocol::caps::ClientCapabilities;
    use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};
    use tokio::net::TcpStream;
    use tokio::task::LocalSet;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_util::sync::CancellationToken;

    const TEST_TOKEN: [u8; crate::auth::TOKEN_LEN] = [0x11; crate::auth::TOKEN_LEN];

    /// A token-gated listener bound to an ephemeral loopback port, with one
    /// known token. TLS is off so the test exercises the token handshake and
    /// frame path without the TLS machinery (covered in `tls`'s own tests).
    ///
    /// The `NamedTempFile` is returned, not dropped: the store re-reads it on
    /// every connection (phux-0d92), so deleting it would revoke every token.
    async fn token_listener() -> (WsListener, SocketAddr, String, tempfile::NamedTempFile) {
        let file = tempfile::NamedTempFile::new().unwrap();
        let token_hex = hex::encode(TEST_TOKEN);
        crate::auth::write_test_credential(file.path(), &TEST_TOKEN);
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
        assert_eq!(peer.transport, TransportType::WebSocket);
        assert_eq!(peer.source_addr, Some(addr.ip()));
        assert_eq!(peer.mcp_host_key.as_deref(), Some("test-credential"));
        let credential = peer
            .credential
            .as_ref()
            .expect("credential retained at boundary");
        assert_eq!(credential.id, "test-credential");
        assert_eq!(credential.principal, "test-principal");
        assert_eq!(credential.scopes, [crate::auth::TERMINAL_CONTROL_SCOPE]);
        assert_eq!(credential.generation, 1);
        assert!(credential.expires_at.is_none());
        assert_eq!(peer.uid, 0);
        assert_eq!(peer.pid, None);
        assert_eq!(peer.exe_path, None);
    }

    /// SPEC §5 has no second framing layer, so a binary message declaring a
    /// `length` that disagrees with its own size is malformed. Before
    /// phux-nwpw the WebSocket reader bounds-checked only the message's total
    /// size, and the dispatch loop's ignored decode tail silently dropped the
    /// surplus — this pins the rejection.
    #[tokio::test]
    async fn websocket_message_with_trailing_bytes_is_rejected() {
        let (listener, addr, token_hex, _tokens) = token_listener().await;

        // Declares a 3-byte body but carries five: two bytes past the frame.
        let overlong: Vec<u8> = vec![0, 0, 0, 3, 0xde, 0xad, 0xbe, 0xff, 0xff];

        let server = async {
            let (mut reader, _writer, _peer) = listener.accept().await.unwrap();
            reader.read_frame().await
        };
        let client = async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            let (mut ws, _resp) =
                tokio_tungstenite::client_async(bearer_request(addr, &token_hex), tcp)
                    .await
                    .expect("valid token must upgrade");
            ws.send(Message::Binary(overlong)).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };

        let (got, ()) = tokio::join!(server, client);
        let err = got.expect_err("a message longer than the frame it declares is malformed");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    /// A peer that connects and then never speaks must not hold the listener.
    ///
    /// `Incoming::accept` runs the upgrade inline and the shared accept loop
    /// awaits it to completion, so before `HANDSHAKE_DEADLINE` a single silent
    /// TCP peer parked the loop forever: the kernel kept completing TCP
    /// handshakes, so every later client connected and then waited for bytes
    /// userspace would never send. Observed in the wild as a server whose
    /// `wss://` and QUIC listeners both went dead while still accepting TCP.
    ///
    /// The clock is paused: tokio auto-advances it while the silent peer keeps
    /// the runtime idle, so this pins the behavior without waiting out the real
    /// deadline.
    #[tokio::test(start_paused = true)]
    async fn a_silent_peer_does_not_wedge_the_listener() {
        let (listener, addr, token_hex, _tokens) = token_listener().await;

        // Connects, completes the TCP handshake, and sends nothing — ever.
        let _silent = TcpStream::connect(addr).await.unwrap();

        let Err(err) = listener.accept().await else {
            panic!("a peer that never speaks must be timed out, not awaited forever");
        };
        let rejection = err
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<WsPeerRejection>())
            .expect("timeout is reported as a typed peer rejection");
        assert_eq!(rejection.stage, WsAcceptStage::Upgrade);

        // The listener is still live: a well-behaved client connecting after
        // the stall is served normally.
        let frame: Vec<u8> = vec![0, 0, 0, 3, 0xde, 0xad, 0xbe];
        let server = async {
            let (mut reader, _writer, _peer) = listener.accept().await.unwrap();
            reader.read_frame().await
        };
        let client = async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            let (mut ws, _resp) =
                tokio_tungstenite::client_async(bearer_request(addr, &token_hex), tcp)
                    .await
                    .expect("valid token must upgrade after a stalled peer");
            ws.send(Message::Binary(frame.clone())).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        let (got, ()) = tokio::join!(server, client);
        assert_eq!(
            got.unwrap().unwrap().as_ref(),
            frame.as_slice(),
            "listener still serves clients after a silent peer is reaped"
        );
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
        let (listener, addr, _token_hex, tokens) = token_listener().await;
        let paired_bytes = [0x33u8; crate::auth::TOKEN_LEN];
        let paired = hex::encode(paired_bytes);

        // Before pairing, the device is a stranger.
        let refused = async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            tokio_tungstenite::client_async(bearer_request(addr, &paired), tcp).await
        };
        let (server_res, client_res) = tokio::join!(listener.accept(), refused);
        assert!(server_res.is_err(), "unpaired device is refused");
        assert!(client_res.is_err());

        // `phux pair` atomically updates the store the server is already
        // serving from. Nothing restarts.
        crate::auth::write_test_credential(tokens.path(), &paired_bytes);

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

        crate::auth::revoke_credential(tokens.path(), "test-credential").unwrap();

        let refused = async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            tokio_tungstenite::client_async(bearer_request(addr, &token_hex), tcp).await
        };
        let (server_res, client_res) = tokio::join!(listener.accept(), refused);
        assert!(server_res.is_err(), "a revoked token no longer upgrades");
        assert!(client_res.is_err());
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one linear end-to-end connection lifecycle is clearer than stateful test helpers"
    )]
    #[tokio::test(flavor = "current_thread")]
    async fn authenticated_attached_session_survives_revocation_then_cleans_up() {
        LocalSet::new()
            .run_until(async {
                let (listener, addr, token_hex, tokens) = token_listener().await;
                let state = crate::state::SharedState::new();
                let root_token = CancellationToken::new();
                crate::runtime::commands::seed_session_with_actor(
                    &state,
                    "authenticated",
                    phux_config::ScrollbackLimits::default(),
                    &root_token,
                )
                .expect("seed attached-session target");

                let accept_state = state.clone();
                let accept_token = root_token.clone();
                let accept_task = tokio::task::spawn_local(async move {
                    crate::runtime::client::accept_loop(&listener, accept_state, accept_token, None)
                        .await
                });

                let tcp = TcpStream::connect(addr).await.unwrap();
                let (mut client, _) =
                    tokio_tungstenite::client_async(bearer_request(addr, &token_hex), tcp)
                        .await
                        .expect("initial credential is admitted");
                let encode = |frame: FrameKind| {
                    let mut encoded = BytesMut::new();
                    frame.encode(&mut encoded);
                    Message::Binary(encoded.to_vec())
                };
                client
                    .send(encode(FrameKind::Hello {
                        client_name: "authenticated-runtime-test".to_owned(),
                        protocol_major: PROTOCOL_VERSION.major,
                        protocol_minor: PROTOCOL_VERSION.minor,
                        protocol_patch: PROTOCOL_VERSION.patch,
                        client_caps: ClientCapabilities::default(),
                    }))
                    .await
                    .unwrap();
                client
                    .send(encode(FrameKind::Attach {
                        attach_id: 1,
                        target: AttachTarget::ByName("authenticated".to_owned()),
                        viewport: ViewportInfo::new(80, 24),
                        request_scrollback: false,
                        scrollback_limit_lines: 0,
                    }))
                    .await
                    .unwrap();

                let mut got_attached = false;
                let mut got_bootstrap = false;
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !(got_attached && got_bootstrap) {
                        let Some(Ok(Message::Binary(data))) = client.next().await else {
                            continue;
                        };
                        match FrameKind::decode(&data).expect("decode runtime frame").0 {
                            FrameKind::Attached { .. } => got_attached = true,
                            FrameKind::BootstrapBegin { .. } => got_bootstrap = true,
                            _ => {}
                        }
                    }
                })
                .await
                .expect("authenticated client attaches through handle_client");

                let client_id = state.with(|server| {
                    assert_eq!(server.attached().len(), 1);
                    assert!(
                        server.idle_since().is_none(),
                        "accept loop records the live authenticated connection"
                    );
                    *server.attached().keys().next().unwrap()
                });
                let credential = state.with(|server| {
                    server
                        .authenticated_credential(client_id)
                        .cloned()
                        .expect("accept loop retains credential attestation")
                });
                assert_eq!(credential.id, "test-credential");
                assert_eq!(credential.principal, "test-principal");
                assert_eq!(credential.scopes, [crate::auth::TERMINAL_CONTROL_SCOPE]);
                assert_eq!(credential.generation, 1);
                assert!(credential.expires_at.is_none());

                crate::auth::revoke_credential(tokens.path(), "test-credential").unwrap();

                // VIEWPORT_RESIZE is meaningful only to an attached client. Its
                // state change proves the established runtime session remains
                // operational without re-authorizing after revocation.
                let resized = ViewportInfo::new(97, 31);
                client
                    .send(encode(FrameKind::ViewportResize { viewport: resized }))
                    .await
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let updated = state.with(|server| {
                            server
                                .attached()
                                .get(&client_id)
                                .and_then(|attached| attached.viewport.as_ref())
                                == Some(&resized)
                        });
                        if updated {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("revoked established session processes attached operation");
                assert_eq!(
                    state.with(|server| server.authenticated_credential(client_id).cloned()),
                    Some(credential),
                    "revocation does not erase the established attestation"
                );

                let reconnect_tcp = TcpStream::connect(addr).await.unwrap();
                let reconnect = tokio_tungstenite::client_async(
                    bearer_request(addr, &token_hex),
                    reconnect_tcp,
                )
                .await
                .expect_err("revoked credential cannot reconnect");
                assert_generic_unauthorized(reconnect);

                client.close(None).await.unwrap();
                drop(client);
                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let cleaned = state.with(|server| {
                            !server.attached().contains_key(&client_id)
                                && server.peer_identity(client_id).is_none()
                                && server.authenticated_credential(client_id).is_none()
                                && server.idle_since().is_some()
                        });
                        if cleaned {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("connection close cleans attachment and credential state");

                root_token.cancel();
                accept_task
                    .await
                    .expect("accept-loop task")
                    .expect("accept loop shuts down cleanly");
            })
            .await;
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
