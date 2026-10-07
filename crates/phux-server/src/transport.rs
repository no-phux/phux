//! Transport abstraction for the accept loop.
//!
//! Every transport yields complete encoded frames (length prefix included,
//! `docs/spec/proto.md` §5, owned by [`phux_protocol::wire::framing`]), so the
//! dispatch loop and codec are transport-agnostic. UDS and the QUIC-class
//! transports carry frames on a byte stream; WebSocket carries exactly one
//! frame per binary message (a size mismatch is malformed, not a batch),
//! ignores text/ping/pong, and treats Close as EOF.

#![allow(
    clippy::future_not_send,
    reason = "single-threaded tokio runtime per ADR-0003; the token-auth accept path captures !Send Rc state and never crosses threads"
)]

pub mod inner_tls;
pub mod quic;
pub mod tls;
#[cfg(feature = "webtransport")]
pub mod webtransport;

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use futures_util::future::LocalBoxFuture;
use futures_util::stream::FuturesUnordered;
use futures_util::{SinkExt, StreamExt};
use phux_protocol::policy::{PeerIdentity, TransportType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream, UnixListener};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::error::CapacityError;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

use phux_protocol::wire::framing::{self, LENGTH_PREFIX_LEN as LENGTH_PREFIX};
pub(crate) const WS_REJECTION_WARN_INTERVAL: Duration = Duration::from_secs(60);

/// How long a peer has to finish each admission step (TLS handshake,
/// WebSocket upgrade, token preamble). Admissions run concurrently, but only
/// [`MAX_PENDING_ADMISSIONS`] at a time, so an un-timed step would let silent
/// peers fill every slot for good.
pub(crate) const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// Connections a listener that admits anyone (a loopback listener with no
/// credential) serves at once: far above any real set of local tools and
/// browser tabs, and a bound on the tasks, sockets, and per-client state a
/// runaway local process can make the server hold. One past it is closed as
/// soon as its handshake completes.
pub(crate) const ANONYMOUS_MAX_CONNECTIONS: usize = 256;

/// Peers one listener admits at once. Finite, but more than one: a silent
/// peer holds one slot until its deadline instead of the whole listener.
pub(crate) const MAX_PENDING_ADMISSIONS: usize = 32;

/// One in-flight admission (TLS, upgrade, token preamble). `None` is a
/// refusal the admission already logged; `Some(Err)` reaches the accept
/// loop's error handling.
pub(crate) type Admission<T> = LocalBoxFuture<'static, Option<io::Result<T>>>;

/// Drives a listener's admissions concurrently, so one slow or silent peer
/// cannot hold every other peer behind its handshake deadline.
pub(crate) struct Admissions<T> {
    pending: tokio::sync::Mutex<FuturesUnordered<Admission<T>>>,
}

impl<T: 'static> Admissions<T> {
    pub(crate) fn new() -> Self {
        Self {
            pending: tokio::sync::Mutex::new(FuturesUnordered::new()),
        }
    }

    /// The next admitted connection. `raw` accepts one raw connection and
    /// returns its admission; it must be cancel-safe, since a completed
    /// admission wins the race against it. Admissions survive a dropped
    /// call, so cancelling this loses no peer.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the set is held for the whole race; its one caller is the accept loop"
    )]
    pub(crate) async fn next<F, Fut>(&self, mut raw: F) -> io::Result<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = io::Result<Admission<T>>>,
    {
        let mut pending = self.pending.lock().await;
        loop {
            tokio::select! {
                done = pending.next(), if !pending.is_empty() => {
                    if let Some(Some(result)) = done {
                        return result;
                    }
                }
                admission = raw(), if pending.len() < MAX_PENDING_ADMISSIONS => {
                    pending.push(admission?);
                }
            }
        }
    }
}

/// Read side of a client connection: yields one complete encoded frame (length
/// prefix included) per call, or `None` at end-of-stream.
pub(crate) trait FrameReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>>;

    /// Logical stream that supplied the most recently returned frame.
    fn frame_origin(&self) -> FrameOrigin {
        FrameOrigin::Control
    }

    /// Take the Terminal-stream event receiver and start the stream mux,
    /// once, after HELLO negotiates `QUIC_STREAMS`. `None` for transports
    /// without streams and on a second call.
    fn take_stream_events(&mut self) -> Option<tokio::sync::mpsc::Receiver<quic::QuicStreamEvent>> {
        None
    }
}

/// Logical stream that supplied a transport frame. Single-stream transports
/// are control-only; the QUIC mux distinguishes Terminal data after upgrade.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FrameOrigin {
    Control,
    Terminal,
}

/// Write side: writes one complete pre-encoded frame.
pub(crate) trait FrameWriter {
    /// Optional bounded telemetry for this transport stream.
    fn stream_tracker(&self) -> Option<crate::stream_diagnostics::StreamTracker> {
        None
    }

    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()>;

    /// Write back-to-back encoded frames; `ends` holds each frame's exclusive
    /// end offset. The default writes one frame per call, which a
    /// message-oriented transport (WebSocket) requires; byte-stream
    /// transports override it with one write of the whole batch.
    async fn write_frames(&mut self, batch: &[u8], ends: &[usize]) -> io::Result<()> {
        let mut start = 0;
        for &end in ends {
            self.write_frame(&batch[start..end]).await?;
            start = end;
        }
        Ok(())
    }

    /// Push buffered writes to the peer, once per mailbox drain. Only
    /// WebSocket buffers; stream transports write straight through.
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

    /// Classify a non-fatal accept error for logging. Narrow the default only
    /// for errors the listener created and recognizes by type.
    fn accept_error_disposition(&self, _error: &io::Error) -> AcceptErrorDisposition {
        AcceptErrorDisposition::Default
    }

    /// Whether an accept error means the incoming source is gone (a dial-out
    /// connector's lost relay leg), not a transient per-connection failure.
    fn accept_errors_are_fatal(&self) -> bool {
        false
    }

    /// Whether each consumer owns its connection and gets the per-Terminal
    /// stream mux (a relay connector shares one tunnel, so it does not).
    fn supports_quic_streams(&self) -> bool {
        false
    }

    /// Most connections this listener serves at once. Only a listener that
    /// admits whoever connects (loopback, no credential) is bounded, at
    /// [`ANONYMOUS_MAX_CONNECTIONS`]; `None` is unbounded.
    fn max_connections(&self) -> Option<usize> {
        None
    }

    /// The transport stamped into `PeerIdentity` and consulted for
    /// transport-gated HELLO features.
    fn transport_type(&self) -> TransportType;
    /// Short transport label for logs (`"uds"` / `"ws"`).
    fn kind(&self) -> &'static str;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AcceptErrorDisposition {
    /// Preserve the shared accept loop's default `ERROR` event.
    Default,
    /// A privacy-safe peer-caused rejection: always logged at `DEBUG`;
    /// `warn_suppressed` is `Some(count)` when a rate-limited warning is due.
    PeerRejected {
        stage: &'static str,
        source_ip: IpAddr,
        warn_suppressed: Option<u64>,
    },
}

// ── Unix domain socket ───────────────────────────────────────────────────────

/// Bytes a stream reader asks for per read. The frame buffer grows only by
/// what actually arrived, so a bare length header never commits a
/// maximum-size frame's memory.
const READ_CHUNK: usize = 64 * 1024;

/// Reassembles length-prefixed frames off a byte stream, cancel-safely.
///
/// The client loop races `read_frame` against other events in a `select!`,
/// which drops the read future whenever another arm wins. Bytes already read
/// live here, not in that future, so a dropped read loses nothing and the
/// stream never desynchronises onto payload bytes.
#[derive(Debug, Default)]
pub(crate) struct FrameAssembler {
    buf: BytesMut,
}

impl FrameAssembler {
    /// The next complete frame (prefix included), or `None` at a clean end of
    /// stream on a frame boundary. Cancel-safe.
    ///
    /// A frame already buffered by an earlier call is returned only after
    /// yielding to the scheduler. One read can carry hundreds of frames (a
    /// typed line is one `INPUT_KEY` per character), and handing them out
    /// with no await point would dispatch the whole burst in one poll while
    /// pane actors and other connections on this thread wait. Input credits
    /// (ADR-0144) keep such a burst lossless; this keeps it fair, as the
    /// per-frame socket reads it replaced were under tokio's cooperative
    /// budget.
    pub(crate) async fn read_frame<R>(&mut self, reader: &mut R) -> io::Result<Option<BytesMut>>
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        let mut read_this_call = false;
        loop {
            if frame_complete(&self.buf)? {
                if !read_this_call {
                    // Before the split, so a cancelled yield loses nothing.
                    tokio::task::yield_now().await;
                }
                return Ok(framing::split_frame(&mut self.buf)?);
            }
            read_this_call = true;
            self.buf.reserve(READ_CHUNK);
            if reader.read_buf(&mut self.buf).await? == 0 {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream ended mid-frame",
                ));
            }
        }
    }
}

/// Whether `buf` starts with one whole frame; a bad length header is the
/// same framing error [`framing::split_frame`] reports.
fn frame_complete(buf: &[u8]) -> Result<bool, framing::FramingError> {
    let Some(header) = buf.first_chunk::<LENGTH_PREFIX>() else {
        return Ok(false);
    };
    Ok(buf.len() >= LENGTH_PREFIX + framing::decode_length(*header)?)
}

/// UDS read half: reassembles length-prefixed frames off the byte stream.
pub(crate) struct UdsReader {
    reader: OwnedReadHalf,
    frames: FrameAssembler,
}

impl FrameReader for UdsReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        self.frames.read_frame(&mut self.reader).await
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

    /// One `write_all` for the whole batch: identical bytes, one syscall.
    async fn write_frames(&mut self, batch: &[u8], _ends: &[usize]) -> io::Result<()> {
        self.writer.write_all(batch).await
    }

    async fn close(&mut self) -> io::Result<()> {
        self.writer.shutdown().await
    }
}

/// UDS listener (a newtype so `Incoming::accept` does not shadow
/// `UnixListener::accept`).
pub(crate) struct UdsListener(UnixListener);

impl UdsListener {
    pub(crate) const fn new(listener: UnixListener) -> Self {
        Self(listener)
    }

    /// The listening descriptor, inherited across a graceful upgrade
    /// (ADR-0032).
    pub(crate) fn as_raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.0.as_raw_fd()
    }
}

impl Incoming for UdsListener {
    type Reader = UdsReader;
    type Writer = UdsWriter;

    fn transport_type(&self) -> TransportType {
        TransportType::UnixSocket
    }

    async fn accept(&self) -> io::Result<(UdsReader, UdsWriter, crate::auth::ConnectionIdentity)> {
        let (stream, _addr) = self.0.accept().await?;
        let peer_identity = peer_identity_from_uds(&stream)?;
        let (reader, writer) = stream.into_split();
        Ok((
            UdsReader {
                reader,
                frames: FrameAssembler::default(),
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

/// The byte stream under a WebSocket: plaintext TCP (loopback only) or TLS
/// (`wss://`, ADR-0031), boxed because `TlsStream` is large.
type ServerStream =
    tokio_util::either::Either<TcpStream, Box<tokio_rustls::server::TlsStream<TcpStream>>>;

type Ws = WebSocketStream<ServerStream>;

/// WebSocket listener: TCP + RFC 6455 upgrade, one binary message per frame.
/// With `tls`, each connection is TLS-wrapped first; with `tokens`, the
/// upgrade must carry a valid pairing token or is refused with HTTP 401.
pub(crate) struct WsListener {
    tcp: TcpListener,
    tls: Option<tokio_rustls::TlsAcceptor>,
    tokens: Option<std::sync::Arc<crate::auth::ReloadingTokenStore>>,
    /// Present only with a workload CA; every admission maps the client
    /// certificate through it, as QUIC does.
    workload: Option<std::sync::Arc<crate::workload::ReloadingWorkloadRegistry>>,
    rejection_warnings: RefusalWarnings,
    admissions: Admissions<WsAccepted>,
    /// Browser origins the anonymous listener admits; unused with tokens.
    origins: std::sync::Arc<AllowedOrigins>,
    /// Live-connection cap: [`ANONYMOUS_MAX_CONNECTIONS`] for the anonymous
    /// listener, none with tokens.
    connection_cap: Option<usize>,
}

type WsAccepted = (WsReader, WsWriter, crate::auth::ConnectionIdentity);

impl WsListener {
    fn from_parts(
        tcp: TcpListener,
        tls: Option<tokio_rustls::TlsAcceptor>,
        tokens: Option<std::sync::Arc<crate::auth::ReloadingTokenStore>>,
        workload: Option<std::sync::Arc<crate::workload::ReloadingWorkloadRegistry>>,
    ) -> Self {
        Self {
            tcp,
            tls,
            tokens,
            workload,
            rejection_warnings: RefusalWarnings::new(),
            admissions: Admissions::new(),
            origins: std::sync::Arc::new(AllowedOrigins::default()),
            connection_cap: None,
        }
    }

    /// This listener with its live-connection cap at `cap` (tests).
    #[cfg(test)]
    pub(crate) const fn with_connection_cap(mut self, cap: usize) -> Self {
        self.connection_cap = Some(cap);
        self
    }

    /// A plaintext loopback listener that still demands a pairing token, for
    /// tests that drive the bearer path without TLS.
    #[cfg(test)]
    pub(crate) async fn loopback_with_tokens(
        tokens: std::sync::Arc<crate::auth::ReloadingTokenStore>,
    ) -> io::Result<Self> {
        let tcp = TcpListener::bind("127.0.0.1:0").await?;
        Ok(Self::from_parts(tcp, None, Some(tokens), None))
    }

    /// Bind a plaintext, unauthenticated listener (loopback browser client)
    /// that admits browser pages only from `origins`.
    pub(crate) async fn bind(addr: SocketAddr, origins: AllowedOrigins) -> io::Result<Self> {
        let mut listener = Self::from_parts(TcpListener::bind(addr).await?, None, None, None);
        listener.origins = std::sync::Arc::new(origins);
        listener.connection_cap = Some(ANONYMOUS_MAX_CONNECTIONS);
        Ok(listener)
    }

    /// Bind a TLS-terminated, token-authenticated listener. The only
    /// production way to attach a token store, so a token never crosses
    /// plaintext (ADR-0031). `workload` is `Some` exactly when `tls` verifies
    /// a workload CA (ADR-0116); the token stays outer admission.
    pub(crate) async fn bind_secure(
        addr: SocketAddr,
        tls: tokio_rustls::TlsAcceptor,
        tokens: std::sync::Arc<crate::auth::ReloadingTokenStore>,
        workload: Option<std::sync::Arc<crate::workload::ReloadingWorkloadRegistry>>,
    ) -> io::Result<Self> {
        Ok(Self::from_parts(
            TcpListener::bind(addr).await?,
            Some(tls),
            Some(tokens),
            workload,
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
                    framing::check_frame(&data)?;
                    return Ok(Some(BytesMut::from(&data[..])));
                }
                // Over the size cap: the same §5 violation as an oversized
                // length header, so the peer gets FRAME_TOO_LARGE.
                Some(Err(WebSocketError::Capacity(CapacityError::MessageTooLong {
                    size, ..
                }))) => {
                    let length =
                        u32::try_from(size.saturating_sub(LENGTH_PREFIX)).unwrap_or(u32::MAX);
                    return Err(framing::FramingError::LengthOutOfRange { length }.into());
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
    /// Queue one frame as one binary message without flushing; the writer
    /// task flushes once per mailbox drain, so a burst leaves as full TCP
    /// segments instead of one partial segment per frame.
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.tx
            .feed(Message::Binary(frame.to_vec().into()))
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

    fn transport_type(&self) -> TransportType {
        TransportType::WebSocket
    }

    async fn accept(&self) -> io::Result<WsAccepted> {
        self.admissions
            .next(|| async {
                let (tcp, peer) = self.tcp.accept().await?;
                let admission = ws_admit(
                    tcp,
                    peer,
                    self.tls.clone(),
                    self.tokens.clone(),
                    self.workload.clone(),
                    std::sync::Arc::clone(&self.origins),
                );
                Ok(Box::pin(async move { Some(admission.await) }) as Admission<WsAccepted>)
            })
            .await
    }

    fn accept_error_disposition(&self, error: &io::Error) -> AcceptErrorDisposition {
        let Some(rejection) = error
            .get_ref()
            .and_then(|source| source.downcast_ref::<WsPeerRejection>())
        else {
            return AcceptErrorDisposition::Default;
        };

        AcceptErrorDisposition::PeerRejected {
            stage: rejection.stage.as_str(),
            source_ip: rejection.source_ip,
            warn_suppressed: self.rejection_warnings.due(),
        }
    }

    fn max_connections(&self) -> Option<usize> {
        self.connection_cap
    }

    fn kind(&self) -> &'static str {
        "ws"
    }
}

/// TLS, then the upgrade and its token gate, each under
/// [`HANDSHAKE_DEADLINE`]; owned inputs so it runs beside other admissions.
async fn ws_admit(
    tcp: TcpStream,
    peer: SocketAddr,
    tls: Option<tokio_rustls::TlsAcceptor>,
    tokens: Option<std::sync::Arc<crate::auth::ReloadingTokenStore>>,
    workload: Option<std::sync::Arc<crate::workload::ReloadingWorkloadRegistry>>,
    origins: std::sync::Arc<AllowedOrigins>,
) -> io::Result<WsAccepted> {
    // Only the source IP is retained; the ephemeral port identifies nothing.
    let source_ip = peer.ip();

    // Nagle would hold each short keystroke echo for the peer's delayed
    // ACK. Failure only costs latency.
    if let Err(err) = tcp.set_nodelay(true) {
        tracing::debug!(error = %err, "could not disable Nagle on accepted WebSocket TCP stream");
    }

    // TLS first, so the token in the upgrade request is encrypted.
    let (stream, peer_leaf) = match &tls {
        Some(acceptor) => tls_handshake(acceptor, tcp, source_ip).await?,
        None => (ServerStream::Left(tcp), None),
    };

    // With a token store, authenticate during the handshake and refuse
    // with HTTP 401 before any phux frame is read.
    let (ws, admitted) = if let Some(store) = &tokens {
        let store = store.clone();
        let workload = workload.clone();
        let captured: std::rc::Rc<std::cell::RefCell<Option<Admitted>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        let sink = captured.clone();
        #[allow(
            clippy::result_large_err,
            reason = "tokio-tungstenite fixes the HTTP rejection response type for its handshake callback"
        )]
        let callback = move |req: &Request, resp| {
            let workload = workload
                .as_deref()
                .map(|registry| (registry, peer_leaf.as_deref()));
            admit_upgrade(req, &store, workload).map_or_else(
                || Err(unauthorized_response()),
                |admitted| {
                    *sink.borrow_mut() = Some(admitted);
                    Ok(select_ws_protocol(req, resp))
                },
            )
        };
        let ws = ws_upgrade(stream, source_ip, callback).await?;
        let admitted = captured.borrow_mut().take();
        (ws, admitted)
    } else {
        #[allow(
            clippy::result_large_err,
            reason = "tokio-tungstenite fixes the HTTP rejection response type for its handshake callback"
        )]
        let callback = move |req: &Request, resp| anonymous_ws_upgrade(req, resp, &origins);
        (ws_upgrade(stream, source_ip, callback).await?, None)
    };
    // The connection's credential (the workload's under workload mTLS)
    // and the pairing-store bearer, kept so its revocation ends the
    // connection live.
    let (credential, bearer) = match admitted {
        Some((credential, bearer)) => (
            Some(credential),
            tokens.as_ref().map(|store| {
                crate::auth::BearerAdmission::new(std::sync::Arc::clone(store), &bearer)
            }),
        ),
        None => (None, None),
    };

    // The credential id rides `mcp_host_key` so policy and audit see a
    // non-anonymous peer.
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
            ssh_origin: None,
            credential,
            bearer,
        },
    ))
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

/// A typed, privacy-safe peer rejection created by [`WsListener`]. It never
/// carries the source error, which may hold header, URI, certificate, or
/// token material; the cause is logged at `debug` where it happens.
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

/// Run the RFC 6455 upgrade under [`HANDSHAKE_DEADLINE`], mapping every
/// failure to a typed stage+IP rejection.
async fn ws_upgrade<C>(stream: ServerStream, source_ip: IpAddr, callback: C) -> io::Result<Ws>
where
    C: tokio_tungstenite::tungstenite::handshake::server::Callback + Unpin,
{
    tokio::time::timeout(
        HANDSHAKE_DEADLINE,
        tokio_tungstenite::accept_hdr_async_with_config(stream, callback, Some(ws_config())),
    )
    .await
    .map_err(|_| {
        tracing::debug!(%source_ip, "WebSocket upgrade timed out");
        ws_accept_error(WsAcceptStage::Upgrade, source_ip)
    })?
    .map_err(|error| classify_ws_upgrade_error(&error, source_ip))
}

/// One WebSocket message carries one frame, so nothing larger than a
/// maximum frame is ever buffered (tungstenite's default is 64 MiB).
fn ws_config() -> tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
    let max = LENGTH_PREFIX + phux_protocol::wire::frame::MAX_FRAME_LEN as usize;
    tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(max))
        .max_frame_size(Some(max))
}

/// A 401 from our callback is the pairing stage; every other failure is the
/// upgrade stage. The tungstenite cause stays at debug.
fn classify_ws_upgrade_error(error: &WebSocketError, source_ip: IpAddr) -> io::Error {
    let stage = classify_ws_upgrade_stage(error);
    tracing::debug!(
        stage = stage.as_str(),
        %source_ip,
        error = %error,
        "WebSocket upgrade rejected"
    );
    ws_accept_error(stage, source_ip)
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

    /// Warn on the first rejection, then at most once per interval, counting
    /// what was suppressed. One listener-wide counter keeps memory bounded.
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

/// One listener's peer refusals, logged at WARN at most once per
/// [`WS_REJECTION_WARN_INTERVAL`] with the count suppressed since, so an
/// unauthenticated peer cannot flood the log. Callers log every refusal at
/// DEBUG regardless.
#[derive(Debug)]
pub(crate) struct RefusalWarnings(Mutex<PeerRejectionWarnLimiter>);

impl RefusalWarnings {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(PeerRejectionWarnLimiter::new()))
    }

    /// Record one refusal; `Some(suppressed)` when a WARN is due now.
    pub(crate) fn due(&self) -> Option<u64> {
        let decision = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .observe(Instant::now());
        match decision {
            PeerRejectionWarnDecision::Suppress => None,
            PeerRejectionWarnDecision::Emit { suppressed } => Some(suppressed),
        }
    }
}

/// Verify either the native Authorization header or the browser's credential
/// subprotocol. Ambiguous carriers are refused before the HTTP upgrade.
fn authorize_request(
    req: &Request,
    store: &crate::auth::ReloadingTokenStore,
) -> Option<crate::auth::AuthenticatedCredential> {
    let token_hex = request_token(req)?;
    if token_hex.len() != crate::auth::TOKEN_LEN * 2 {
        return None;
    }
    let token = hex::decode(token_hex).ok()?;
    store.authenticate_and_touch(&token)
}

/// What an admitted upgrade carries: the connection's credential (the
/// workload's under workload mTLS, else the bearer's), then the bearer.
type Admitted = (
    crate::auth::AuthenticatedCredential,
    crate::auth::AuthenticatedCredential,
);

/// Admit a WebSocket upgrade: the pairing token first, then, with a workload
/// CA, the client certificate through the registry's current generation.
/// Every refusal is the same 401 (`workload-auth.md` §3, §7).
fn admit_upgrade(
    req: &Request,
    store: &crate::auth::ReloadingTokenStore,
    workload: Option<(&crate::workload::ReloadingWorkloadRegistry, Option<&[u8]>)>,
) -> Option<Admitted> {
    let bearer = authorize_request(req, store)?;
    let Some((registry, leaf)) = workload else {
        return Some((bearer.clone(), bearer));
    };
    registry
        .lookup_certificate(leaf?)
        .map(|workload| (workload, bearer))
}

/// Terminate TLS under [`HANDSHAKE_DEADLINE`], keeping the client's leaf
/// certificate, if it sent one, for the workload registry lookup.
async fn tls_handshake(
    acceptor: &tokio_rustls::TlsAcceptor,
    tcp: TcpStream,
    source_ip: IpAddr,
) -> io::Result<(
    ServerStream,
    Option<rustls::pki_types::CertificateDer<'static>>,
)> {
    let tls = tokio::time::timeout(HANDSHAKE_DEADLINE, acceptor.accept(tcp))
        .await
        .map_err(|_| {
            tracing::debug!(%source_ip, "WebSocket TLS handshake timed out");
            ws_accept_error(WsAcceptStage::TlsHandshake, source_ip)
        })?
        .map_err(|err| {
            tracing::debug!(%source_ip, error = %err, "WebSocket TLS handshake failed");
            ws_accept_error(WsAcceptStage::TlsHandshake, source_ip)
        })?;
    let leaf = peer_leaf_certificate(&tls);
    Ok((ServerStream::Right(Box::new(tls)), leaf))
}

/// The leaf certificate a TLS client presented (chain already verified).
fn peer_leaf_certificate(
    tls: &tokio_rustls::server::TlsStream<TcpStream>,
) -> Option<rustls::pki_types::CertificateDer<'static>> {
    tls.get_ref()
        .1
        .peer_certificates()?
        .first()
        .map(|leaf| leaf.clone().into_owned())
}

fn request_token(req: &Request) -> Option<&str> {
    let (browser_protocol, browser_token) = browser_auth_protocols(req)?;
    let mut headers = req.headers().get_all("authorization").iter();
    let header = headers.next();
    if headers.next().is_some() || (header.is_some() && browser_token.is_some()) {
        return None;
    }
    match header {
        Some(header) => {
            let header = header.to_str().ok()?;
            Some(
                header
                    .strip_prefix("Bearer ")
                    .or_else(|| header.strip_prefix("bearer "))?
                    .trim(),
            )
        }
        None if browser_protocol => browser_token,
        None => None,
    }
}

/// Parse the small browser auth vocabulary without copying credential bytes.
fn browser_auth_protocols(req: &Request) -> Option<(bool, Option<&str>)> {
    let mut application = false;
    let mut bearer = None;
    for header in req.headers().get_all("sec-websocket-protocol") {
        for protocol in header.to_str().ok()?.split(',').map(str::trim) {
            if protocol == "phux.v1" {
                if application {
                    return None;
                }
                application = true;
            } else if let Some(token) = protocol.strip_prefix("phux.bearer.")
                && bearer.replace(token).is_some()
            {
                return None;
            }
        }
    }
    Some((application, bearer))
}

/// Select only the public application protocol. Never echo the auth carrier.
fn select_ws_protocol(req: &Request, mut response: Response) -> Response {
    if matches!(browser_auth_protocols(req), Some((true, _))) {
        response.headers_mut().insert(
            "sec-websocket-protocol",
            tokio_tungstenite::tungstenite::http::HeaderValue::from_static("phux.v1"),
        );
    }
    response
}

/// A browser asking for paired authentication must not accidentally establish
/// an anonymous connection when the listener has no authority store, and a
/// page from an origin the listener does not admit is refused outright.
#[allow(
    clippy::result_large_err,
    reason = "tungstenite fixes the HTTP upgrade callback error type"
)]
fn anonymous_ws_upgrade(
    req: &Request,
    response: Response,
    origins: &AllowedOrigins,
) -> Result<Response, ErrorResponse> {
    if !origins.admits_request(req) {
        return Err(forbidden_origin_response());
    }
    match browser_auth_protocols(req) {
        Some((_, None)) => Ok(select_ws_protocol(req, response)),
        _ => Err(unauthorized_response()),
    }
}

/// Browser origins the anonymous WebSocket listener admits.
///
/// Browsers attach `Origin` to every WebSocket handshake but do not hold it
/// to the same-origin policy, so without this any web page the user visits
/// could open `ws://127.0.0.1:PORT` and drive a shell (cross-site WebSocket
/// hijacking). A request with no `Origin` (a native client) or a loopback one
/// (a locally served page) is admitted, plus whatever
/// `PHUX_WS_ALLOWED_ORIGINS` names: a comma-separated list of exact origins,
/// or `*` for a listener fronted by a proxy that checks origins itself.
/// `null` (sandboxed frames, `file:` pages) is never loopback.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AllowedOrigins {
    any: bool,
    exact: Vec<String>,
}

impl AllowedOrigins {
    /// Parse `PHUX_WS_ALLOWED_ORIGINS`; unset admits loopback origins only.
    #[must_use]
    pub(crate) fn parse(raw: Option<&str>) -> Self {
        let mut origins = Self::default();
        for entry in raw.unwrap_or_default().split(',').map(str::trim) {
            if entry == "*" {
                origins.any = true;
            } else if !entry.is_empty() {
                origins
                    .exact
                    .push(entry.trim_end_matches('/').to_ascii_lowercase());
            }
        }
        origins
    }

    fn admits_request(&self, req: &Request) -> bool {
        let mut values = req.headers().get_all("origin").iter();
        let Some(value) = values.next() else {
            return true;
        };
        if values.next().is_some() {
            return false;
        }
        value.to_str().is_ok_and(|origin| self.admits(origin))
    }

    fn admits(&self, origin: &str) -> bool {
        let origin = origin.trim().to_ascii_lowercase();
        self.any || self.exact.contains(&origin) || is_loopback_origin(&origin)
    }
}

/// Whether a serialized origin (`scheme://host[:port]`) names this machine.
fn is_loopback_origin(origin: &str) -> bool {
    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    if rest.contains(['/', '@', '?', '#']) {
        return false;
    }
    let host = match rest.strip_prefix('[') {
        Some(bracketed) => match bracketed.split_once(']') {
            Some((host, tail)) if tail.is_empty() || tail.starts_with(':') => host,
            _ => return false,
        },
        None => rest.split_once(':').map_or(rest, |(host, _)| host),
    };
    host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The HTTP 403 for a browser page from an origin the listener refuses.
fn forbidden_origin_response() -> ErrorResponse {
    use tokio_tungstenite::tungstenite::http::StatusCode;
    let mut resp = ErrorResponse::new(Some("origin not allowed".to_owned()));
    *resp.status_mut() = StatusCode::FORBIDDEN;
    resp
}

/// The generic HTTP 401 for any pairing failure, leaking nothing about which
/// check failed.
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

    use phux_protocol::caps::ClientCapabilities;
    use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};
    use tokio::net::TcpStream;
    use tokio::task::LocalSet;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_util::sync::CancellationToken;

    const TEST_TOKEN: [u8; crate::auth::TOKEN_LEN] = [0x11; crate::auth::TOKEN_LEN];
    /// One complete framed message: 4-byte length prefix (body = 3) + body.
    const FRAME: [u8; 7] = [0, 0, 0, 3, 0xde, 0xad, 0xbe];

    /// A plaintext token-gated listener with one known token. The token file
    /// is returned because the store re-reads it on every connection.
    async fn token_listener() -> (WsListener, SocketAddr, String, tempfile::NamedTempFile) {
        let file = tempfile::NamedTempFile::new().unwrap();
        crate::auth::write_test_credential(file.path(), &TEST_TOKEN);
        let store = crate::auth::ReloadingTokenStore::load(file.path().to_path_buf()).unwrap();
        let listener = WsListener::loopback_with_tokens(Arc::new(store))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, addr, hex::encode(TEST_TOKEN), file)
    }

    fn request(addr: SocketAddr) -> Request {
        format!("ws://{addr}/").into_client_request().unwrap()
    }

    /// A `ws://` upgrade request carrying `Authorization: Bearer <hex>`.
    fn bearer_request(addr: SocketAddr, token_hex: &str) -> Request {
        let mut req = request(addr);
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {token_hex}").parse().unwrap(),
        );
        req
    }

    fn browser_request(addr: SocketAddr, protocols: &str) -> Request {
        let mut req = request(addr);
        req.headers_mut()
            .insert("sec-websocket-protocol", protocols.parse().unwrap());
        req
    }

    /// Upgrade with `request`, send `messages`, and return what the server
    /// read for each, the admitted identity, and the upgrade response.
    async fn exchange(
        listener: &WsListener,
        addr: SocketAddr,
        request: Request,
        messages: &[&[u8]],
    ) -> (
        Vec<io::Result<Option<BytesMut>>>,
        crate::auth::ConnectionIdentity,
        tokio_tungstenite::tungstenite::handshake::client::Response,
    ) {
        let server = async {
            let (mut reader, _writer, peer) = listener.accept().await.unwrap();
            let mut reads = Vec::new();
            for _ in messages {
                reads.push(reader.read_frame().await);
            }
            (reads, peer)
        };
        let client = async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            let (mut ws, response) = tokio_tungstenite::client_async(request, tcp)
                .await
                .expect("upgrade");
            for message in messages {
                ws.send(Message::Binary(message.to_vec().into()))
                    .await
                    .unwrap();
            }
            // Hold the connection open until the server has read.
            tokio::time::sleep(Duration::from_millis(50)).await;
            response
        };
        let ((reads, peer), response) = tokio::join!(server, client);
        (reads, peer, response)
    }

    async fn refused_handshake(
        listener: &WsListener,
        addr: SocketAddr,
        request: Request,
    ) -> (String, WebSocketError) {
        let client = async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            tokio_tungstenite::client_async(request, tcp).await
        };
        let (server_result, client_result) = tokio::join!(listener.accept(), client);
        let Err(server_error) = server_result else {
            panic!("server unexpectedly admitted rejected handshake");
        };
        let Err(client_error) = client_result else {
            panic!("client unexpectedly completed rejected handshake");
        };
        (server_error.to_string(), client_error)
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

    /// A bearer upgrade round-trips a frame and stamps the credential; a
    /// message longer than the frame it declares is malformed, not a batch.
    #[tokio::test]
    async fn valid_token_upgrades_and_rejects_trailing_bytes() {
        let (listener, addr, token_hex, _tokens) = token_listener().await;
        let overlong = [0, 0, 0, 3, 0xde, 0xad, 0xbe, 0xff, 0xff];
        let (reads, peer, _) = exchange(
            &listener,
            addr,
            bearer_request(addr, &token_hex),
            &[&FRAME, &overlong],
        )
        .await;
        let mut reads = reads.into_iter();
        assert_eq!(reads.next().unwrap().unwrap().unwrap().as_ref(), &FRAME);
        let err = reads.next().unwrap().expect_err("overlong message");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        assert_eq!(peer.transport, TransportType::WebSocket);
        assert_eq!(peer.source_addr, Some(addr.ip()));
        assert_eq!(peer.mcp_host_key.as_deref(), Some("test-credential"));
        assert_eq!(
            (peer.uid, peer.pid, peer.exe_path.as_ref()),
            (0, None, None)
        );
        let credential = peer.credential.as_ref().expect("credential retained");
        assert_eq!(credential.id, "test-credential");
        assert_eq!(credential.principal, "test-principal");
        assert_eq!(credential.scopes, [crate::auth::TERMINAL_CONTROL_SCOPE]);
        assert_eq!(credential.generation, 1);
        assert!(credential.expires_at.is_none());
    }

    #[tokio::test]
    async fn browser_subprotocol_auth_stamps_identity_without_echoing_secret() {
        let (listener, addr, token, _tokens) = token_listener().await;
        let request = browser_request(addr, &format!("phux.v1, phux.bearer.{token}"));
        let (reads, peer, response) = exchange(&listener, addr, request, &[&FRAME]).await;
        assert_eq!(response.headers()["sec-websocket-protocol"], "phux.v1");
        assert!(!format!("{:?}", response.headers()).contains(&token));
        assert_eq!(
            reads[0].as_ref().unwrap().as_ref().unwrap().as_ref(),
            &FRAME
        );
        assert_eq!(peer.credential.unwrap().id, "test-credential");
    }

    #[tokio::test]
    async fn browser_auth_rejects_ambiguous_and_malformed_carriers() {
        let (listener, addr, token, _tokens) = token_listener().await;
        for protocols in [
            format!("phux.bearer.{token}"),
            "phux.v1, phux.bearer.not-hex".to_owned(),
            format!("phux.v1, phux.bearer.{token}, phux.bearer.{token}"),
            format!("phux.v1, phux.v1, phux.bearer.{token}"),
            "phux.v1".to_owned(),
        ] {
            let (error, response) =
                refused_handshake(&listener, addr, browser_request(addr, &protocols)).await;
            assert_generic_unauthorized(response);
            assert!(!error.contains(&token));
        }
        let mut mixed = bearer_request(addr, &token);
        mixed.headers_mut().insert(
            "sec-websocket-protocol",
            format!("phux.v1, phux.bearer.{token}").parse().unwrap(),
        );
        let mut repeated = bearer_request(addr, &token);
        repeated
            .headers_mut()
            .append("authorization", format!("Bearer {token}").parse().unwrap());
        for request in [mixed, repeated] {
            let (_, response) = refused_handshake(&listener, addr, request).await;
            assert_generic_unauthorized(response);
        }
    }

    /// The anonymous loopback listener serves at most its connection cap at
    /// once (256 in production): one past it is closed as soon as it is
    /// admitted, and a slot a connection gives back is reused.
    #[tokio::test(flavor = "current_thread")]
    async fn the_anonymous_listener_caps_live_connections() {
        assert_eq!(
            WsListener::bind("127.0.0.1:0".parse().unwrap(), AllowedOrigins::default())
                .await
                .unwrap()
                .max_connections(),
            Some(ANONYMOUS_MAX_CONNECTIONS)
        );
        LocalSet::new()
            .run_until(async {
                let listener =
                    WsListener::bind("127.0.0.1:0".parse().unwrap(), AllowedOrigins::default())
                        .await
                        .unwrap()
                        .with_connection_cap(2);
                let addr = listener.local_addr().unwrap();
                let state = crate::state::SharedState::new();
                let root_token = CancellationToken::new();
                let accept_state = state.clone();
                let accept_token = root_token.clone();
                let accept_task = tokio::task::spawn_local(async move {
                    crate::runtime::client::accept_loop(&listener, accept_state, accept_token, None)
                        .await
                });
                let connect = || async move {
                    let tcp = TcpStream::connect(addr).await.unwrap();
                    tokio_tungstenite::client_async(request(addr), tcp)
                        .await
                        .expect("the upgrade completes")
                        .0
                };
                // Closed by the server within the window, or still open.
                let closed_within = |mut client: WebSocketStream<TcpStream>, window| async move {
                    let closed = matches!(
                        tokio::time::timeout(window, client.next()).await,
                        Ok(None | Some(Err(_) | Ok(Message::Close(_))))
                    );
                    (closed, client)
                };
                let window = Duration::from_millis(500);

                let first = connect().await;
                let (closed, second) = closed_within(connect().await, window).await;
                assert!(!closed, "the second connection is within the cap");
                let (closed, _third) = closed_within(connect().await, window).await;
                assert!(closed, "a connection past the cap is closed");

                drop(first);
                let mut reused = false;
                for _ in 0..20 {
                    let (closed, client) = closed_within(connect().await, window).await;
                    if !closed {
                        reused = true;
                        drop(client);
                        break;
                    }
                }
                assert!(reused, "a returned slot is reused");
                drop(second);
                root_token.cancel();
                let _ = accept_task.await;
            })
            .await;
    }

    #[tokio::test]
    async fn anonymous_listener_refuses_browser_credentials_but_accepts_public_protocol() {
        let listener = WsListener::bind("127.0.0.1:0".parse().unwrap(), AllowedOrigins::default())
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let token = hex::encode(TEST_TOKEN);
        let (_, response) = refused_handshake(
            &listener,
            addr,
            browser_request(addr, &format!("phux.v1, phux.bearer.{token}")),
        )
        .await;
        assert_generic_unauthorized(response);

        let (_, identity, response) =
            exchange(&listener, addr, browser_request(addr, "phux.v1"), &[]).await;
        assert!(identity.credential.is_none());
        assert_eq!(response.headers()["sec-websocket-protocol"], "phux.v1");
    }

    /// A web page the user happens to visit cannot drive the anonymous
    /// loopback listener: its handshake carries a foreign `Origin` and is
    /// refused. Native clients (no `Origin`) and locally served pages still
    /// connect, and an operator can name extra origins.
    #[tokio::test]
    async fn anonymous_listener_refuses_foreign_browser_origins() {
        let with_origin = |addr: SocketAddr, origin: &str| {
            let mut req = request(addr);
            req.headers_mut().insert("origin", origin.parse().unwrap());
            req
        };
        let listener = WsListener::bind("127.0.0.1:0".parse().unwrap(), AllowedOrigins::default())
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        for origin in [
            "https://evil.example",
            "null",
            "http://127.0.0.1.evil.example",
        ] {
            let (_, error) = refused_handshake(&listener, addr, with_origin(addr, origin)).await;
            let WebSocketError::Http(response) = error else {
                panic!("{origin}: expected an HTTP refusal");
            };
            assert_eq!(
                response.status(),
                tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN,
                "{origin}"
            );
        }
        for origin in [
            "http://127.0.0.1:4321",
            "http://localhost:8080",
            "https://[::1]",
        ] {
            exchange(&listener, addr, with_origin(addr, origin), &[]).await;
        }
        exchange(&listener, addr, request(addr), &[]).await;

        let fronted = WsListener::bind(
            "127.0.0.1:0".parse().unwrap(),
            AllowedOrigins::parse(Some("https://phux.sh")),
        )
        .await
        .unwrap();
        let addr = fronted.local_addr().unwrap();
        exchange(&fronted, addr, with_origin(addr, "https://phux.sh"), &[]).await;
    }

    #[test]
    fn origin_policy_parses_lists_wildcards_and_loopback_forms() {
        let policy = AllowedOrigins::parse(Some(" https://a.example/ , https://B.example"));
        assert!(policy.admits("https://a.example"));
        assert!(policy.admits("https://b.example"));
        assert!(!policy.admits("https://c.example"));
        assert!(AllowedOrigins::parse(Some("*")).admits("https://c.example"));
        let loopback = AllowedOrigins::default();
        for admitted in [
            "http://localhost",
            "http://127.0.0.2:9",
            "https://[::1]:443",
        ] {
            assert!(loopback.admits(admitted), "{admitted}");
        }
        for refused in [
            "null",
            "file://",
            "http://localhost.evil.example",
            "http://evil.example#@127.0.0.1",
            "http://[::1].evil.example",
            "ws://127.0.0.1",
        ] {
            assert!(!loopback.admits(refused), "{refused}");
        }
    }

    /// One mTLS WebSocket attempt: the admitted identity, or `None` when the
    /// listener refused it (and the client's dial then failed).
    async fn mtls_attempt(
        listener: &WsListener,
        url: &str,
        token: &str,
        identity: &phux_dial::TlsClientIdentity,
    ) -> Option<crate::auth::ConnectionIdentity> {
        let dial = phux_dial::WsDial {
            url: url.to_owned(),
            token: Some(token.to_owned()),
            trust: phux_dial::CertTrust::SkipVerify,
            tls_server_name: None,
            identity: None,
        };
        let (accepted, dialed) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(
                listener.accept(),
                phux_dial::ws::dial_with_identity(&dial, identity)
            )
        })
        .await
        .expect("the attempt settles");
        let admitted = accepted.ok().map(|(_, _, admitted)| admitted);
        assert_eq!(dialed.is_ok(), admitted.is_some());
        admitted
    }

    /// With a workload CA, WSS maps the client certificate through the
    /// registry exactly as QUIC does: the token stays outer admission, an
    /// enrolled certificate becomes the credential, and a missing
    /// certificate, wrong token, or revoked credential is refused live.
    #[tokio::test]
    async fn wss_with_a_configured_ca_looks_up_the_client_certificate_like_quic() {
        use crate::workload::{
            ClientMaterial, ReloadingWorkloadRegistry, WorkloadPaths, WorkloadRegistry,
        };
        let dir = tempfile::tempdir().unwrap();
        let leaf = dir.path().join("leaf.pem");
        let leaf_key = dir.path().join("leaf-key.pem");
        crate::transport::tls::ensure_self_signed_for(&leaf, &leaf_key, &["localhost".to_owned()])
            .unwrap();
        let paths = WorkloadPaths {
            ca_cert: dir.path().join("ca.pem"),
            ca_key: dir.path().join("ca.key"),
            registry: dir.path().join("workload-keys"),
        };
        crate::workload::init_authority(&paths.ca_cert, &paths.ca_key).unwrap();

        let client_key = rcgen::KeyPair::generate().unwrap();
        let csr = rcgen::CertificateParams::new(vec!["client".to_owned()])
            .unwrap()
            .serialize_request(&client_key)
            .unwrap();
        let material = ClientMaterial::from_pem(csr.pem().unwrap().as_bytes()).unwrap();
        let expires = chrono::Utc::now().timestamp() + 3600;
        let prepared = crate::workload::prepare_enrollment(&paths, &material, expires).unwrap();
        let client_cert = dir.path().join("client.pem");
        let client_key_path = dir.path().join("client.key");
        std::fs::write(&client_cert, prepared.issued_chain_pem().unwrap()).unwrap();
        std::fs::write(&client_key_path, client_key.serialize_pem()).unwrap();
        let enrolled = prepared
            .commit(&paths.registry, vec!["*@global".to_owned()], expires)
            .unwrap();

        let tokens = dir.path().join("tokens.json");
        crate::auth::write_test_credential(&tokens, &TEST_TOKEN);
        let acceptor = crate::transport::tls::acceptor_from_pem_with_client_ca(
            &leaf,
            &leaf_key,
            Some(&crate::workload::authority_certificate(&paths.ca_cert).unwrap()),
        )
        .unwrap();
        let listener = WsListener::bind_secure(
            "127.0.0.1:0".parse().unwrap(),
            acceptor,
            Arc::new(crate::auth::ReloadingTokenStore::load(tokens).unwrap()),
            Some(Arc::new(
                ReloadingWorkloadRegistry::load(paths.registry.clone()).unwrap(),
            )),
        )
        .await
        .unwrap();
        let url = format!("wss://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let token = hex::encode(TEST_TOKEN);
        let enrolled_identity = phux_dial::TlsClientIdentity::PemFiles {
            certificate: client_cert,
            private_key: client_key_path,
        };

        let admitted = mtls_attempt(&listener, &url, &token, &enrolled_identity)
            .await
            .expect("an enrolled certificate with a valid token is admitted");
        let credential = admitted.credential.as_ref().expect("workload credential");
        assert_eq!(credential.id, enrolled.id);
        assert_eq!(credential.generation, 1);
        assert_eq!(credential.scopes, ["*@global"]);
        assert_eq!(admitted.mcp_host_key.as_deref(), Some(enrolled.id.as_str()));

        let anonymous = phux_dial::TlsClientIdentity::None;
        assert!(
            mtls_attempt(&listener, &url, &token, &anonymous)
                .await
                .is_none(),
            "no client certificate"
        );
        let wrong_token = hex::encode([0x22; crate::auth::TOKEN_LEN]);
        assert!(
            mtls_attempt(&listener, &url, &wrong_token, &enrolled_identity)
                .await
                .is_none(),
            "the pairing token stays outer admission"
        );
        WorkloadRegistry::revoke(&paths.registry, &enrolled.id).unwrap();
        assert!(
            mtls_attempt(&listener, &url, &token, &enrolled_identity)
                .await
                .is_none(),
            "revocation applies to the next connection without a restart"
        );
    }

    /// A peer that connects and never speaks is timed out rather than
    /// wedging the listener, which then serves the next client.
    #[tokio::test(start_paused = true)]
    async fn a_silent_peer_does_not_wedge_the_listener() {
        let (listener, addr, token_hex, _tokens) = token_listener().await;
        let _silent = TcpStream::connect(addr).await.unwrap();

        let Err(err) = listener.accept().await else {
            panic!("a peer that never speaks must be timed out");
        };
        let rejection = err
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<WsPeerRejection>())
            .expect("timeout is reported as a typed peer rejection");
        assert_eq!(rejection.stage, WsAcceptStage::Upgrade);

        // Paused time would let a healthy upgrade lose to the virtual
        // deadline while the kernel is briefly pending.
        tokio::time::resume();
        let (reads, _, _) =
            exchange(&listener, addr, bearer_request(addr, &token_hex), &[&FRAME]).await;
        assert_eq!(
            reads[0].as_ref().unwrap().as_ref().unwrap().as_ref(),
            &FRAME
        );
    }

    /// A message larger than any frame is refused as a framing violation
    /// without the server buffering it whole first.
    #[tokio::test]
    async fn an_oversized_message_is_a_framing_violation() {
        let (listener, addr, token_hex, _tokens) = token_listener().await;
        let max = phux_protocol::wire::frame::MAX_FRAME_LEN as usize;
        let mut oversized = vec![0_u8; LENGTH_PREFIX + max + 1];
        oversized[..LENGTH_PREFIX].copy_from_slice(&3_u32.to_be_bytes());
        let server = async {
            let (mut reader, _writer, _peer) = listener.accept().await.unwrap();
            reader.read_frame().await
        };
        let client = async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            let (mut ws, _) =
                tokio_tungstenite::client_async(bearer_request(addr, &token_hex), tcp)
                    .await
                    .expect("upgrade");
            // The server stops reading partway, so the send may break.
            let _ = ws.send(Message::Binary(oversized.into())).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let (read, ()) = tokio::join!(server, client);
        let err = read.expect_err("an oversized message");
        let violation = err
            .get_ref()
            .and_then(|source| source.downcast_ref::<framing::FramingError>())
            .expect("a typed framing violation");
        assert!(
            matches!(violation, framing::FramingError::LengthOutOfRange { .. }),
            "{violation:?}"
        );
    }

    /// A peer that connects and never speaks holds one admission slot, not
    /// the listener: a healthy peer behind it upgrades at once instead of
    /// after the silent one's deadline.
    #[tokio::test]
    async fn a_silent_peer_does_not_delay_other_admissions() {
        let (listener, addr, token_hex, _tokens) = token_listener().await;
        let _silent = TcpStream::connect(addr).await.unwrap();
        let (reads, _, _) = tokio::time::timeout(
            Duration::from_secs(3),
            exchange(&listener, addr, bearer_request(addr, &token_hex), &[&FRAME]),
        )
        .await
        .expect("the healthy peer is admitted without waiting on the silent one");
        assert_eq!(
            reads[0].as_ref().unwrap().as_ref().unwrap().as_ref(),
            &FRAME
        );
    }

    #[tokio::test]
    async fn invalid_malformed_and_missing_tokens_are_identical_safe_rejections() {
        let (listener, addr, _token_hex, _tokens) = token_listener().await;
        let wrong = hex::encode([0x22u8; crate::auth::TOKEN_LEN]);
        let mut errors = Vec::new();
        for request in [
            bearer_request(addr, &wrong),
            bearer_request(addr, "not-hex"),
            request(addr),
        ] {
            let (error, response) = refused_handshake(&listener, addr, request).await;
            assert_generic_unauthorized(response);
            errors.push(error);
        }
        let expected = format!(
            "WebSocket pairing authentication rejected (source_ip={})",
            addr.ip()
        );
        for error in &errors {
            assert_eq!(error, &expected);
        }
        assert!(!expected.contains(&addr.port().to_string()));
    }

    /// ADR-0081: `phux pair` needs no restart, so a token minted after the
    /// listener bound upgrades against the running listener.
    #[tokio::test]
    async fn a_token_minted_after_bind_upgrades_without_a_restart() {
        let (listener, addr, _token_hex, tokens) = token_listener().await;
        let paired_bytes = [0x33u8; crate::auth::TOKEN_LEN];
        let paired = hex::encode(paired_bytes);

        let (_, response) = refused_handshake(&listener, addr, bearer_request(addr, &paired)).await;
        assert_generic_unauthorized(response);

        crate::auth::write_test_credential(tokens.path(), &paired_bytes);
        exchange(&listener, addr, bearer_request(addr, &paired), &[]).await;
    }

    /// ADR-0116: revoking a bearer ends its established session with the
    /// workload-auth §7 goodbye, refuses reconnection, and cleans up state.
    #[allow(
        clippy::too_many_lines,
        reason = "one linear end-to-end connection lifecycle is clearer than stateful test helpers"
    )]
    #[tokio::test(flavor = "current_thread")]
    async fn bearer_revoke_now_terminates_live_sessions() {
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
                crate::runtime::revocation::spawn_revocation_watcher(&state, &root_token);

                let tcp = TcpStream::connect(addr).await.unwrap();
                let (mut client, _) =
                    tokio_tungstenite::client_async(bearer_request(addr, &token_hex), tcp)
                        .await
                        .expect("initial credential is admitted");
                let encode = |frame: FrameKind| {
                    let mut encoded = BytesMut::new();
                    frame.encode(&mut encoded);
                    Message::Binary(encoded.to_vec().into())
                };
                client
                    .send(encode(FrameKind::Hello {
                        client_name: "authenticated-runtime-test".to_owned(),
                        protocol_major: phux_protocol::PROTOCOL_VERSION.major,
                        protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
                        protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
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
                        role_policy: None,
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
                    assert!(server.idle_since().is_none());
                    *server.attached().keys().next().unwrap()
                });
                let credential = state.with(|server| {
                    server
                        .authenticated_credential(client_id)
                        .cloned()
                        .expect("accept loop retains credential attestation")
                });
                assert_eq!(credential.id, "test-credential");

                crate::auth::revoke_credential(tokens.path(), "test-credential").unwrap();

                let mut ending = Vec::new();
                tokio::time::timeout(Duration::from_secs(5), async {
                    while let Some(message) = client.next().await {
                        match message {
                            Ok(Message::Binary(data)) => ending
                                .push(FrameKind::decode(&data).expect("decode runtime frame").0),
                            Ok(Message::Close(_)) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                })
                .await
                .expect("a revoked bearer's live session is closed");
                assert!(
                    matches!(
                        ending.as_slice(),
                        [
                            ..,
                            FrameKind::Error {
                                request_id: None,
                                code: phux_protocol::wire::frame::ErrorCode::PermissionDenied,
                                ..
                            },
                            FrameKind::Detached {
                                reason: Some(
                                    phux_protocol::wire::frame::DetachReason::AuthorizationRevoked
                                ),
                                ..
                            },
                        ]
                    ),
                    "the goodbye is ERROR then DETACHED, and nothing follows it: {ending:?}"
                );

                let reconnect_tcp = TcpStream::connect(addr).await.unwrap();
                let reconnect = tokio_tungstenite::client_async(
                    bearer_request(addr, &token_hex),
                    reconnect_tcp,
                )
                .await
                .expect_err("revoked credential cannot reconnect");
                assert_generic_unauthorized(reconnect);

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
        use tokio_tungstenite::tungstenite::http;
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
        assert_eq!(
            (typed.stage, typed.source_ip),
            (WsAcceptStage::TlsHandshake, source_ip)
        );

        let auth_error = WebSocketError::Http(Box::new(
            http::Response::builder()
                .status(http::StatusCode::UNAUTHORIZED)
                .body(None::<Vec<u8>>)
                .unwrap(),
        ));
        assert_eq!(
            classify_ws_upgrade_stage(&auth_error),
            WsAcceptStage::PairingAuthentication
        );

        let unsafe_underlying = WebSocketError::Protocol(
            tokio_tungstenite::tungstenite::error::ProtocolError::InvalidHeader(Box::new(
                "authorization".parse().unwrap(),
            )),
        );
        assert_eq!(
            classify_ws_upgrade_error(&unsafe_underlying, source_ip).to_string(),
            "WebSocket upgrade failed (source_ip=192.0.2.41)"
        );
    }

    #[test]
    fn peer_rejection_warning_limiter_is_global_bounded_and_deterministic() {
        let start = Instant::now();
        let mut limiter = PeerRejectionWarnLimiter::new();
        let interval = WS_REJECTION_WARN_INTERVAL;
        for (at, want) in [
            (start, PeerRejectionWarnDecision::Emit { suppressed: 0 }),
            (
                start + interval.saturating_sub(Duration::from_nanos(1)),
                PeerRejectionWarnDecision::Suppress,
            ),
            (
                start + interval,
                PeerRejectionWarnDecision::Emit { suppressed: 1 },
            ),
            (start + interval, PeerRejectionWarnDecision::Suppress),
            (
                start + interval * 2,
                PeerRejectionWarnDecision::Emit { suppressed: 1 },
            ),
        ] {
            assert_eq!(limiter.observe(at), want);
        }
        limiter.suppressed = u64::MAX;
        assert_eq!(
            limiter.observe(start + interval * 2),
            PeerRejectionWarnDecision::Suppress
        );
        assert_eq!(limiter.suppressed, u64::MAX);
    }

    /// The client loop drops `read_frame` whenever another `select!` arm
    /// wins. A frame split across that drop must still arrive whole, or the
    /// stream desynchronises and payload bytes are parsed as frames.
    #[tokio::test]
    async fn a_dropped_read_loses_no_bytes() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (server_read, _server_write) = server.into_split();
        let mut reader = UdsReader {
            reader: server_read,
            frames: FrameAssembler::default(),
        };
        let mut client = client;
        let frame = [0_u8, 0, 0, 6, 0xaa, 1, 2, 3, 4, 5];
        client.write_all(&frame[..7]).await.unwrap();
        let cancelled = tokio::time::timeout(Duration::from_millis(50), reader.read_frame()).await;
        assert!(cancelled.is_err(), "half a frame must not complete a read");
        client.write_all(&frame[7..]).await.unwrap();
        let read = tokio::time::timeout(Duration::from_secs(5), reader.read_frame())
            .await
            .expect("the rest of the frame completes the read")
            .unwrap()
            .expect("one frame");
        assert_eq!(&read[..], &frame[..]);
        drop(client);
        assert!(reader.read_frame().await.unwrap().is_none(), "clean EOF");
    }

    /// A bare length header commits no more memory than the bytes that
    /// actually arrived: a peer cannot pin a maximum-size frame buffer with
    /// four bytes.
    #[tokio::test]
    async fn a_bare_header_does_not_reserve_the_whole_frame() {
        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let (server_read, _server_write) = server.into_split();
        let mut reader = UdsReader {
            reader: server_read,
            frames: FrameAssembler::default(),
        };
        let max = phux_protocol::wire::frame::MAX_FRAME_LEN;
        client.write_all(&max.to_be_bytes()).await.unwrap();
        let pending = tokio::time::timeout(Duration::from_millis(50), reader.read_frame()).await;
        assert!(pending.is_err());
        assert!(
            reader.frames.buf.capacity() < 1024 * 1024,
            "reserved {} bytes for a 4-byte header",
            reader.frames.buf.capacity()
        );
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
