//! QUIC transport ([ADR-0007]): the identical length-prefixed phux frames
//! (`docs/spec/proto.md` §5) over one bidirectional QUIC stream per
//! connection, plus the per-Terminal stream multiplexer (§4.2).
//!
//! TLS 1.3 is intrinsic to QUIC. Routable consumers authenticate with a
//! bearer-token **preamble** (`len: u32 BE` + token) as the first bytes of
//! the stream, validated inside `Incoming::accept` before any frame is read:
//! the QUIC analogue of the WebSocket `Authorization` header (ADR-0031).
//!
//! [ADR-0007]: ../../../docs/adr/0007-mosh-class-transport-and-satellites.md

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::BytesMut;
use phux_dial::window::{SendWindow, TrackedSend};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::wire::framing;
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

use super::{FrameReader, FrameWriter, Incoming, LENGTH_PREFIX};

/// Upper bound on the token preamble body: room for a longer future token,
/// never a large allocation.
const MAX_TOKEN_PREAMBLE: usize = 256;

/// Idle timeout for a connection with no traffic and no keep-alive.
pub(super) const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Keep-alive interval, well under [`IDLE_TIMEOUT`], so a quiet attached
/// client survives NATs.
pub(super) const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// QUIC application close code for a connection refused at admission.
const AUTH_FAILED_CODE: u32 = 0x01;

/// Cap on client-opened bidi streams per connection (proto.md §4.2): far
/// above panes-per-attach, far below task exhaustion. quinn enforces it.
const MAX_CONCURRENT_BIDI_STREAMS: u64 = 128;

/// Bound on each admission step (handshake, first stream, preamble), so a
/// stalled peer frees its admission slot.
const ADMISSION_DEADLINE: Duration = super::HANDSHAKE_DEADLINE;

type QuicAccepted = (QuicMuxReader, QuicWriter, crate::auth::ConnectionIdentity);

/// A QUIC listener: a quinn endpoint bound to a UDP socket.
pub(crate) struct QuicListener {
    endpoint: quinn::Endpoint,
    admission: QuicAdmission,
    workload_registry: Option<Arc<crate::workload::ReloadingWorkloadRegistry>>,
    admissions: super::Admissions<QuicAccepted>,
    /// Refused preambles, warned about at a bounded rate.
    refusals: Arc<super::RefusalWarnings>,
}

/// Whom a [`QuicListener`] admits.
#[derive(Clone)]
pub(crate) enum QuicAdmission {
    /// Anyone who completes the TLS handshake; no preamble (loopback/dev).
    Open,
    /// A bearer token from the pairing store (ADR-0031).
    Store(Arc<crate::auth::ReloadingTokenStore>),
    /// Only the token this listener was opened with (`OPEN_LISTENER`,
    /// ADR-0120); pairing-store tokens are refused.
    Listener(Arc<crate::auth::ListenerToken>),
}

impl QuicListener {
    /// Bind a listener that requires a pairing-store token when `tokens` is
    /// `Some`, and, with `client_ca`, a client certificate that maps to an
    /// active credential in `workload_registry`.
    pub(crate) fn from_pem_with_client_ca_and_registry(
        addr: SocketAddr,
        cert_path: &std::path::Path,
        key_path: &std::path::Path,
        tokens: Option<Arc<crate::auth::ReloadingTokenStore>>,
        client_ca: Option<&rustls::pki_types::CertificateDer<'static>>,
        workload_registry: Option<Arc<crate::workload::ReloadingWorkloadRegistry>>,
    ) -> Result<Self, QuicBindError> {
        let tls = super::tls::quic_server_config_with_client_ca(cert_path, key_path, client_ca)?;
        Ok(Self {
            endpoint: server_endpoint(addr, tls, Some(MAX_CONCURRENT_BIDI_STREAMS))?,
            admission: tokens.map_or(QuicAdmission::Open, QuicAdmission::Store),
            workload_registry,
            admissions: super::Admissions::new(),
            refusals: Arc::new(super::RefusalWarnings::new()),
        })
    }

    /// Bind a listener that admits whoever `admission` names, with the same
    /// workload-CA verification as the configured listener when `workload`
    /// is given.
    pub(crate) fn with_admission(
        addr: SocketAddr,
        cert_path: &std::path::Path,
        key_path: &std::path::Path,
        admission: QuicAdmission,
        workload: Option<(
            &rustls::pki_types::CertificateDer<'static>,
            Arc<crate::workload::ReloadingWorkloadRegistry>,
        )>,
    ) -> Result<Self, QuicBindError> {
        let (client_ca, registry) = workload.unzip();
        let mut listener = Self::from_pem_with_client_ca_and_registry(
            addr, cert_path, key_path, None, client_ca, registry,
        )?;
        listener.admission = admission;
        Ok(listener)
    }

    pub(crate) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }
}

/// Errors from constructing a QUIC-based listener.
#[derive(Debug, thiserror::Error)]
pub(crate) enum QuicBindError {
    /// Building the rustls/QUIC crypto config failed.
    #[error("quic tls: {0}")]
    Tls(#[from] super::tls::TlsError),
    /// The QUIC crypto config had no usable initial cipher suite.
    #[error("quic crypto: {0}")]
    Crypto(#[from] quinn::crypto::rustls::NoInitialCipherSuite),
    /// Binding the UDP endpoint failed.
    #[error("quic bind: {0}")]
    Io(#[from] io::Error),
}

/// A quinn server endpoint with phux's idle/keep-alive policy and, when
/// given, a cap on client-opened bidi streams.
pub(super) fn server_endpoint(
    addr: SocketAddr,
    tls: rustls::ServerConfig,
    max_bidi_streams: Option<u64>,
) -> Result<quinn::Endpoint, QuicBindError> {
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    let mut transport = quinn::TransportConfig::default();
    if let Ok(idle) = IDLE_TIMEOUT.try_into() {
        transport.max_idle_timeout(Some(idle));
    }
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    if let Some(cap) = max_bidi_streams.and_then(|cap| quinn::VarInt::from_u64(cap).ok()) {
        transport.max_concurrent_bidi_streams(cap);
    }
    server_config.transport_config(Arc::new(transport));
    Ok(quinn::Endpoint::server(server_config, addr)?)
}

/// The registry credential for a connection's client certificate, looked up
/// in the registry generation current right now (rustls already verified
/// the chain), so enrollment and revocation apply to the next connection.
fn workload_credential(
    conn: &quinn::Connection,
    registry: &crate::workload::ReloadingWorkloadRegistry,
) -> Option<crate::auth::AuthenticatedCredential> {
    let identity = conn.peer_identity()?;
    let certs = identity.downcast_ref::<Vec<rustls::pki_types::CertificateDer<'static>>>()?;
    registry.lookup_certificate(certs.first()?.as_ref())
}

/// QUIC read half: length-prefixed frames off one receive stream.
pub(crate) struct QuicReader {
    recv: quinn::RecvStream,
    frames: super::FrameAssembler,
}

impl QuicReader {
    /// Wrap one already-authenticated QUIC receive stream in phux framing.
    pub(crate) fn from_stream(recv: quinn::RecvStream) -> Self {
        Self {
            recv,
            frames: super::FrameAssembler::default(),
        }
    }
}

impl FrameReader for QuicReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        self.frames.read_frame(&mut self.recv).await
    }
}

/// QUIC write half. Writes go through [`TrackedSend`], which holds quinn's
/// send window near the congestion window so a slow path backs up into the
/// mailbox and the output pump resyncs instead of replaying a backlog.
pub(crate) struct QuicWriter {
    send: TrackedSend<quinn::SendStream>,
    finished: bool,
    diagnostics: crate::stream_diagnostics::StreamRegistration,
}

impl QuicWriter {
    /// Wrap one already-authenticated send stream. `window` belongs to the
    /// connection: build it once and clone it per stream.
    pub(crate) fn from_stream(send: quinn::SendStream, window: SendWindow) -> Self {
        Self::with_lane(send, window, crate::stream_diagnostics::StreamLane::Control)
    }

    pub(crate) fn from_terminal_stream(send: quinn::SendStream, window: SendWindow) -> Self {
        Self::with_lane(
            send,
            window,
            crate::stream_diagnostics::StreamLane::Terminal,
        )
    }

    fn with_lane(
        send: quinn::SendStream,
        window: SendWindow,
        lane: crate::stream_diagnostics::StreamLane,
    ) -> Self {
        use std::hash::BuildHasher;
        // Quinn's stable id is address-derived; hash it with a process-random
        // key so snapshots correlate streams without exposing an address.
        static IDS: std::sync::OnceLock<std::collections::hash_map::RandomState> =
            std::sync::OnceLock::new();
        let connection_id = IDS
            .get_or_init(std::collections::hash_map::RandomState::new)
            .hash_one(window.connection().stable_id());
        let diagnostics =
            crate::stream_diagnostics::register(crate::stream_diagnostics::StreamContext {
                connection_id,
                stream_id: u64::from(send.id()),
                lane,
            });
        diagnostics.tracker().set_active(true);
        Self {
            send: TrackedSend::new(send, window),
            finished: false,
            diagnostics,
        }
    }

    pub(crate) fn diagnostic_tracker(&self) -> crate::stream_diagnostics::StreamTracker {
        self.diagnostics.tracker()
    }
}

impl FrameWriter for QuicWriter {
    fn stream_tracker(&self) -> Option<crate::stream_diagnostics::StreamTracker> {
        Some(self.diagnostic_tracker())
    }

    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        let _write = self.diagnostics.tracker().begin_write();
        self.send.write_all(frame).await
    }

    /// One `write_all` for the whole batch: a byte stream whose frames carry
    /// their own length prefix, so the bytes are identical and a burst costs
    /// one write instead of one per frame.
    async fn write_frames(&mut self, batch: &[u8], _ends: &[usize]) -> io::Result<()> {
        let _write = self.diagnostics.tracker().begin_write();
        self.send.write_all(batch).await
    }

    #[allow(
        clippy::unused_async_trait_impl,
        reason = "FrameWriter requires an async close operation, while Quinn's finish is synchronous"
    )]
    async fn close(&mut self) -> io::Result<()> {
        self.send.get_mut().finish().map_err(io::Error::other)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for QuicWriter {
    fn drop(&mut self) {
        if !self.finished {
            // Cancellation may interrupt a length-prefixed frame. An explicit
            // reset distinguishes that abandoned write from an orderly FIN.
            let _ = self.send.get_mut().reset(0x10_u32.into());
        }
    }
}

impl Incoming for QuicListener {
    type Reader = QuicMuxReader;
    type Writer = QuicWriter;

    fn transport_type(&self) -> TransportType {
        TransportType::Quic
    }

    fn supports_quic_streams(&self) -> bool {
        true
    }

    /// One endpoint multiplexes many connections, so a failed or refused
    /// connection is logged and skipped; only endpoint closure ends the loop.
    async fn accept(&self) -> io::Result<QuicAccepted> {
        self.admissions
            .next(|| async {
                let incoming = self.endpoint.accept().await.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotConnected, "quic endpoint closed")
                })?;
                let admission = admit_connection(
                    incoming,
                    self.admission.clone(),
                    self.workload_registry.clone(),
                    Arc::clone(&self.refusals),
                );
                Ok(Box::pin(async move { admission.await.map(Ok) })
                    as super::Admission<QuicAccepted>)
            })
            .await
    }

    fn max_connections(&self) -> Option<usize> {
        matches!(self.admission, QuicAdmission::Open).then_some(super::ANONYMOUS_MAX_CONNECTIONS)
    }

    fn kind(&self) -> &'static str {
        "quic"
    }
}

/// Handshake, first stream, preamble, then workload certificate, each
/// bounded by [`ADMISSION_DEADLINE`]. `None` means refused or failed.
async fn admit_connection(
    incoming: quinn::Incoming,
    admission: QuicAdmission,
    workload_registry: Option<Arc<crate::workload::ReloadingWorkloadRegistry>>,
    refusals: Arc<super::RefusalWarnings>,
) -> Option<QuicAccepted> {
    let remote = incoming.remote_address();
    let conn = match tokio::time::timeout(ADMISSION_DEADLINE, incoming).await {
        Ok(Ok(conn)) => conn,
        Ok(Err(err)) => {
            debug!(%remote, error = %err, "quic handshake failed");
            return None;
        }
        Err(_) => {
            debug!(%remote, "quic handshake timed out");
            return None;
        }
    };
    let (send, mut recv) = match tokio::time::timeout(ADMISSION_DEADLINE, conn.accept_bi()).await {
        Ok(Ok(pair)) => pair,
        Ok(Err(err)) => {
            debug!(%remote, error = %err, "quic stream accept failed");
            return None;
        }
        Err(_) => {
            debug!(%remote, "quic stream accept timed out");
            conn.close(AUTH_FAILED_CODE.into(), b"stream timeout");
            return None;
        }
    };

    let credential = match &admission {
        QuicAdmission::Open => None,
        admission => {
            let preamble =
                tokio::time::timeout(ADMISSION_DEADLINE, admit(&mut recv, admission)).await;
            let Ok(Some(credential)) = preamble else {
                if preamble.is_err() {
                    debug!(%remote, "quic auth preamble timed out");
                } else if let Some(suppressed) = refusals.due() {
                    warn!(
                        %remote,
                        suppressed,
                        "quic consumer refused: missing or invalid token"
                    );
                } else {
                    debug!(%remote, "quic consumer refused: missing or invalid token");
                }
                conn.close(AUTH_FAILED_CODE.into(), b"unauthorized");
                return None;
            };
            Some(credential)
        }
    };

    // One refusal for every workload failure; the peer learns only that
    // it was refused (`workload-auth.md` §7).
    let workload_credential = match &workload_registry {
        Some(registry) => {
            let Some(credential) = workload_credential(&conn, registry) else {
                debug!(%remote, "quic mTLS client identity refused");
                conn.close(AUTH_FAILED_CODE.into(), b"unauthorized");
                return None;
            };
            Some(credential)
        }
        None => None,
    };
    let bearer = bearer_admission(&admission, credential.as_ref());
    let credential = workload_credential.or(credential);
    let peer = PeerIdentity {
        uid: 0,
        pid: None,
        exe_path: None,
        mcp_host_key: credential.as_ref().map(|credential| credential.id.clone()),
        transport: TransportType::Quic,
        source_addr: Some(remote.ip()),
    };

    let window = SendWindow::new(conn.clone());
    let writer = QuicWriter::from_stream(send, window.clone());
    let mut reader = QuicMuxReader::new(recv, conn, window);
    reader.diagnostics = Some(writer.diagnostic_tracker());
    Some((
        reader,
        writer,
        crate::auth::ConnectionIdentity {
            peer,
            credential,
            ssh_origin: None,
            bearer,
        },
    ))
}

/// Read the token preamble and verify it against the pairing store.
pub(crate) async fn authorize_preamble(
    recv: &mut quinn::RecvStream,
    store: &crate::auth::ReloadingTokenStore,
) -> Option<crate::auth::AuthenticatedCredential> {
    let (_, token) = read_length_prefixed(recv, MAX_TOKEN_PREAMBLE).await?;
    store.authenticate_and_touch(&token)
}

/// Read the token preamble and verify it against whoever `admission` names.
async fn admit(
    recv: &mut quinn::RecvStream,
    admission: &QuicAdmission,
) -> Option<crate::auth::AuthenticatedCredential> {
    match admission {
        QuicAdmission::Open => None,
        QuicAdmission::Store(store) => authorize_preamble(recv, store).await,
        QuicAdmission::Listener(token) => {
            let (_, preamble) = read_length_prefixed(recv, MAX_TOKEN_PREAMBLE).await?;
            token.authenticate(&preamble)
        }
    }
}

/// The pairing-store admission retained so its bearer's revocation ends the
/// connection live. A listener token dies with its listener (ADR-0120).
fn bearer_admission(
    admission: &QuicAdmission,
    credential: Option<&crate::auth::AuthenticatedCredential>,
) -> Option<crate::auth::BearerAdmission> {
    match (admission, credential) {
        (QuicAdmission::Store(store), Some(credential)) => Some(crate::auth::BearerAdmission::new(
            Arc::clone(store),
            credential,
        )),
        _ => None,
    }
}

/// Read `len: u32 BE` then that many bytes, or `None` when missing, empty,
/// longer than `max`, or truncated.
async fn read_length_prefixed(
    recv: &mut quinn::RecvStream,
    max: usize,
) -> Option<([u8; LENGTH_PREFIX], Vec<u8>)> {
    let mut len_buf = [0u8; LENGTH_PREFIX];
    if !read_exact_quic(recv, &mut len_buf).await.ok()? {
        return None;
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > max {
        return None;
    }
    let mut body = vec![0u8; len];
    if !read_exact_quic(recv, &mut body).await.ok()? {
        return None;
    }
    Some((len_buf, body))
}

/// Depth of the mux's merged frame channel (control plus every Terminal
/// stream). Per-stream isolation lives in QUIC flow control, not here.
const MUX_FRAME_CHANNEL: usize = 64;
const MAX_WIRE_FRAME_BYTES: usize =
    phux_protocol::wire::frame::MAX_FRAME_LEN as usize + LENGTH_PREFIX;
/// Two full legal frames (one Terminal, one control), each with its length
/// prefix: rounding down would deadlock one valid maximum frame.
const MUX_FRAME_BYTES: usize = 2 * MAX_WIRE_FRAME_BYTES;
/// Terminal streams may hold at most one maximum frame of the budget, so
/// control traffic always has room.
const MUX_TERMINAL_FRAME_BYTES: usize = MAX_WIRE_FRAME_BYTES;
/// Once a header announces a body, it must arrive within this bound, so a
/// peer cannot reserve byte permits indefinitely.
const FRAME_BODY_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub(crate) struct AdmittedFrame {
    bytes: BytesMut,
    _connection_bytes: Option<tokio::sync::OwnedSemaphorePermit>,
    _terminal_bytes: Option<tokio::sync::OwnedSemaphorePermit>,
    origin: Option<Arc<AtomicBool>>,
    event: Option<QuicStreamEvent>,
    _queue_ticket: Option<crate::stream_diagnostics::QueueTicket>,
}

impl AdmittedFrame {
    fn event(event: QuicStreamEvent) -> Self {
        Self {
            bytes: BytesMut::new(),
            _connection_bytes: None,
            _terminal_bytes: None,
            origin: None,
            event: Some(event),
            _queue_ticket: None,
        }
    }
}

/// Depth of the Terminal-stream event channel; binds and ends are rare.
const MUX_EVENT_CHANNEL: usize = 32;
/// Half-open streams that may concurrently wait for `STREAM_BIND`.
const MAX_PENDING_STREAM_BINDS: usize = 16;

/// Application error code resetting a refused or malformed Terminal stream.
const BIND_REFUSED_CODE: u32 = 0x10;

/// Reset a Terminal stream unread (proto.md §4.2). The uncorrelated `ERROR`
/// on control carries the reason.
pub(crate) fn refuse_terminal_stream(mut send: quinn::SendStream, mut recv: quinn::RecvStream) {
    let _ = send.reset(quinn::VarInt::from_u32(BIND_REFUSED_CODE));
    let _ = recv.stop(quinn::VarInt::from_u32(BIND_REFUSED_CODE));
}

/// A Terminal stream's lifecycle event (proto.md §4.2, ADR-0115).
#[derive(Debug)]
pub(crate) enum QuicStreamEvent {
    /// A well-formed `STREAM_BIND` arrived; admission belongs to the client
    /// task, which receives everything needed to start the stream's pump.
    Bound {
        /// The Terminal whose §4 frames ride this stream.
        terminal_id: phux_protocol::ids::ResourceId,
        /// The stream generation this stream opens under.
        stream_id: phux_protocol::ids::StreamId,
        /// This stream's send half, for the per-stream writer.
        send: quinn::SendStream,
        /// Receive half, unread until bind authorization succeeds.
        recv: quinn::RecvStream,
        /// The connection-scoped congestion tracker.
        window: SendWindow,
        /// Merged destination used after bind admission.
        frames: tokio::sync::mpsc::Sender<AdmittedFrame>,
        /// Connection-wide queued/incomplete frame byte budget.
        frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
        /// Terminal-only share of that budget.
        terminal_frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
    },
    /// A bound stream ended cleanly: the detach signal.
    Ended {
        /// The Terminal the ended stream carried.
        terminal_id: phux_protocol::ids::ResourceId,
        /// The generation the ended stream opened under.
        stream_id: phux_protocol::ids::StreamId,
    },
    /// A bound stream ended because framing or transport failed; the typed
    /// cause lets the runtime report the right protocol code.
    Failed {
        terminal_id: phux_protocol::ids::ResourceId,
        stream_id: phux_protocol::ids::StreamId,
        failure: QuicStreamFailure,
    },
}

#[derive(Debug)]
pub(crate) enum QuicStreamFailure {
    Framing(framing::FramingError),
    IncompleteFrame,
    Transport(String),
}

/// QUIC read half with multi-stream upgrade (`docs/spec/proto.md` §4.2).
///
/// Before [`FrameReader::take_stream_events`] it reads the control stream
/// directly; afterwards control and every bound Terminal stream pump into
/// one merged, byte-budgeted channel. Dropping the reader aborts the pumps.
pub(crate) struct QuicMuxReader {
    control: Option<QuicReader>,
    conn: quinn::Connection,
    window: SendWindow,
    frames_rx: Option<tokio::sync::mpsc::Receiver<AdmittedFrame>>,
    control_open: bool,
    control_done: Option<tokio::sync::oneshot::Receiver<io::Result<()>>>,
    tasks: tokio::task::JoinSet<()>,
    last_origin: crate::transport::FrameOrigin,
    stream_events_tx: Option<tokio::sync::mpsc::Sender<QuicStreamEvent>>,
    // `read_frame` is cancelled whenever runtime handles another select arm.
    // Keep dequeued lifecycle events here until their destination is reserved.
    pending_event: Option<QuicStreamEvent>,
    diagnostics: Option<crate::stream_diagnostics::StreamTracker>,
}

impl QuicMuxReader {
    pub(crate) fn new(
        recv: quinn::RecvStream,
        conn: quinn::Connection,
        window: SendWindow,
    ) -> Self {
        Self {
            control: Some(QuicReader::from_stream(recv)),
            conn,
            window,
            frames_rx: None,
            control_open: true,
            control_done: None,
            tasks: tokio::task::JoinSet::new(),
            last_origin: crate::transport::FrameOrigin::Control,
            stream_events_tx: None,
            pending_event: None,
            diagnostics: None,
        }
    }
}

impl FrameReader for QuicMuxReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        let Some(rx) = self.frames_rx.as_mut() else {
            // Pre-upgrade: the upgrade takes `control` with it, so it is
            // present exactly here.
            let Some(reader) = self.control.as_mut() else {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "quic mux upgraded without its control stream",
                ));
            };
            self.last_origin = crate::transport::FrameOrigin::Control;
            return reader.read_frame().await;
        };
        loop {
            if !self.control_open {
                return Ok(None);
            }
            if self.pending_event.is_some() {
                let Some(events) = self.stream_events_tx.as_ref() else {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "quic mux lost its stream event channel",
                    ));
                };
                let Ok(permit) = events.reserve().await else {
                    return Ok(None);
                };
                if let Some(event) = self.pending_event.take() {
                    permit.send(event);
                }
            }
            tokio::select! {
                biased;
                done = async {
                    match self.control_done.as_mut() {
                        Some(rx) => rx.await,
                        None => core::future::pending().await,
                    }
                } => {
                    // Control end is connection end, even with live Terminal
                    // streams: nothing can continue without control.
                    self.control_open = false;
                    return done.map_or_else(|_| Ok(None), |result| result.map(|()| None));
                }
                frame = rx.recv() => {
                    let Some(admitted) = frame else {
                        return Ok(None);
                    };
                    if let Some(event) = admitted.event {
                        self.pending_event = Some(event);
                        continue;
                    }
                    // A retired Terminal stream's queued frames are dropped.
                    if admitted.origin.as_ref().is_some_and(|origin| {
                        !origin.load(Ordering::Acquire)
                    }) {
                        continue;
                    }
                    self.last_origin = if admitted.origin.is_some() {
                        crate::transport::FrameOrigin::Terminal
                    } else {
                        crate::transport::FrameOrigin::Control
                    };
                    return Ok(Some(admitted.bytes));
                },
            }
        }
    }

    fn frame_origin(&self) -> crate::transport::FrameOrigin {
        self.last_origin
    }

    fn take_stream_events(&mut self) -> Option<tokio::sync::mpsc::Receiver<QuicStreamEvent>> {
        if self.frames_rx.is_some() {
            return None;
        }
        let (frames_tx, frames_rx) = tokio::sync::mpsc::channel(MUX_FRAME_CHANNEL);
        let frame_bytes = std::sync::Arc::new(tokio::sync::Semaphore::new(MUX_FRAME_BYTES));
        let terminal_frame_bytes =
            std::sync::Arc::new(tokio::sync::Semaphore::new(MUX_TERMINAL_FRAME_BYTES));
        let (events_tx, events_rx) = tokio::sync::mpsc::channel(MUX_EVENT_CHANNEL);
        self.stream_events_tx = Some(events_tx.clone());
        let (control_done_tx, control_done_rx) = tokio::sync::oneshot::channel();
        let mut control = self.control.take()?;
        let conn = self.conn.clone();
        let window = self.window.clone();
        let control_frames_tx = frames_tx.clone();
        let control_frame_bytes = frame_bytes.clone();
        let diagnostics = self.diagnostics.clone();
        self.tasks.spawn(async move {
            loop {
                let read = read_framed_bounded(
                    &mut control.recv,
                    &control_frame_bytes,
                    None,
                    None,
                    diagnostics.as_ref(),
                )
                .await;
                match read {
                    Ok(Some(frame)) => {
                        if control_frames_tx.send(frame).await.is_err() {
                            return;
                        }
                    }
                    Ok(None) => {
                        let _ = control_done_tx.send(Ok(()));
                        return;
                    }
                    Err(err) => {
                        let _ = control_done_tx.send(Err(err));
                        return;
                    }
                }
            }
        });
        self.tasks.spawn(accept_terminal_streams(
            conn,
            window,
            frames_tx,
            events_tx,
            frame_bytes,
            terminal_frame_bytes,
        ));
        self.frames_rx = Some(frames_rx);
        self.control_done = Some(control_done_rx);
        Some(events_rx)
    }
}

/// Accept client-opened Terminal streams for one upgraded connection. Each
/// must open with a well-formed `STREAM_BIND` within [`ADMISSION_DEADLINE`]
/// or is reset unread; well-formed binds go to the client task as `Bound`.
async fn accept_terminal_streams(
    conn: quinn::Connection,
    window: SendWindow,
    frames_tx: tokio::sync::mpsc::Sender<AdmittedFrame>,
    events_tx: tokio::sync::mpsc::Sender<QuicStreamEvent>,
    frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
    terminal_frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
) {
    let pending = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_STREAM_BINDS));
    let mut bind_tasks = tokio::task::JoinSet::new();
    loop {
        let accepted = tokio::select! {
            accepted = conn.accept_bi() => accepted,
            Some(_) = bind_tasks.join_next(), if !bind_tasks.is_empty() => continue,
        };
        let Ok((send, mut recv)) = accepted else {
            return;
        };
        let Ok(permit) = pending.clone().try_acquire_owned() else {
            debug!("quic terminal stream refused: pending bind capacity exhausted");
            refuse_terminal_stream(send, recv);
            continue;
        };
        let window = window.clone();
        let frames = frames_tx.clone();
        let events = events_tx.clone();
        let frame_bytes = frame_bytes.clone();
        let terminal_frame_bytes = terminal_frame_bytes.clone();
        bind_tasks.spawn(async move {
            let _permit = permit;
            let bind = tokio::time::timeout(ADMISSION_DEADLINE, read_stream_bind(&mut recv)).await;
            let Some(bind) = bind.ok().flatten() else {
                debug!("quic terminal stream refused: no well-formed STREAM_BIND within deadline");
                refuse_terminal_stream(send, recv);
                return;
            };
            let event = QuicStreamEvent::Bound {
                terminal_id: bind.terminal_id,
                stream_id: bind.stream_id,
                send,
                recv,
                window,
                frames,
                frame_bytes,
                terminal_frame_bytes,
            };
            let _ = events.send(event).await;
        });
    }
}

/// Pump one bound Terminal stream's frames into the merged channel until the
/// stream ends, then emit `Ended` (or `Failed`).
#[allow(
    clippy::too_many_arguments,
    clippy::significant_drop_tightening,
    reason = "the admitted stream owns distinct transport, provenance, cancellation, and two-level budget handles; frame permits intentionally live through channel admission"
)]
pub(crate) async fn pump_terminal_stream(
    mut recv: quinn::RecvStream,
    terminal_id: phux_protocol::ids::ResourceId,
    stream_id: phux_protocol::ids::StreamId,
    frames_tx: tokio::sync::mpsc::Sender<AdmittedFrame>,
    frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
    terminal_frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
    active: Arc<AtomicBool>,
    cancelled: tokio_util::sync::CancellationToken,
    diagnostics: Option<crate::stream_diagnostics::StreamTracker>,
) {
    loop {
        let read = tokio::select! {
            () = cancelled.cancelled() => return,
            read = read_framed_bounded(
                &mut recv,
                &frame_bytes,
                Some(&terminal_frame_bytes),
                Some(active.clone()),
                diagnostics.as_ref(),
            ) => read,
        };
        let failure = match read {
            Ok(Some(frame)) if terminal_frame_matches(&frame.bytes, &terminal_id, stream_id) => {
                let closed = tokio::select! {
                    () = cancelled.cancelled() => true,
                    result = frames_tx.send(frame) => result.is_err(),
                };
                if closed {
                    return;
                }
                continue;
            }
            Ok(Some(_)) => {
                debug!(
                    ?terminal_id,
                    ?stream_id,
                    "frame refused on mismatched Terminal stream"
                );
                Some(QuicStreamFailure::Transport(
                    "frame provenance did not match STREAM_BIND".to_owned(),
                ))
            }
            Ok(None) => None,
            Err(err) => Some(stream_failure(&err)),
        };
        let event = match failure {
            None => QuicStreamEvent::Ended {
                terminal_id,
                stream_id,
            },
            Some(failure) => QuicStreamEvent::Failed {
                terminal_id,
                stream_id,
                failure,
            },
        };
        tokio::select! {
            () = cancelled.cancelled() => {}
            _ = frames_tx.send(AdmittedFrame::event(event)) => {}
        }
        return;
    }
}

fn stream_failure(err: &io::Error) -> QuicStreamFailure {
    if let Some(framing) = err
        .get_ref()
        .and_then(|source| source.downcast_ref::<framing::FramingError>())
    {
        return QuicStreamFailure::Framing(*framing);
    }
    if err.kind() == io::ErrorKind::TimedOut || err.kind() == io::ErrorKind::UnexpectedEof {
        return QuicStreamFailure::IncompleteFrame;
    }
    QuicStreamFailure::Transport(err.to_string())
}

/// Verify the stream's provenance before a frame can enter shared dispatch:
/// only Terminal-scoped input/ack frames for the bound Terminal (and, where
/// the frame names one, the bound generation) are allowed.
fn terminal_frame_matches(
    framed: &[u8],
    terminal_id: &phux_protocol::ids::ResourceId,
    stream_id: phux_protocol::ids::StreamId,
) -> bool {
    use phux_protocol::wire::frame::FrameKind;
    let Ok((frame, tail)) = FrameKind::decode(framed) else {
        return false;
    };
    if !tail.is_empty() {
        return false;
    }
    match frame {
        FrameKind::InputKey {
            terminal_id: id, ..
        }
        | FrameKind::InputMouse {
            terminal_id: id, ..
        }
        | FrameKind::InputFocus {
            terminal_id: id, ..
        }
        | FrameKind::InputPaste {
            terminal_id: id, ..
        }
        | FrameKind::InputTerminalReply {
            terminal_id: id, ..
        }
        | FrameKind::ResizeTerminal {
            terminal_id: id, ..
        } => id == *terminal_id,
        FrameKind::FrameAck {
            terminal_id: id,
            stream_id: generation,
            ..
        }
        | FrameKind::HistoryRequest {
            terminal_id: id,
            stream_id: generation,
            ..
        } => id == *terminal_id && generation == stream_id,
        _ => false,
    }
}

/// Read one `STREAM_BIND` header off a fresh Terminal stream.
async fn read_stream_bind(
    recv: &mut quinn::RecvStream,
) -> Option<phux_protocol::wire::stream_bind::StreamBind> {
    let (len_buf, body) = read_length_prefixed(
        recv,
        phux_protocol::wire::stream_bind::MAX_STREAM_BIND_BYTES,
    )
    .await?;
    let mut framed = Vec::with_capacity(LENGTH_PREFIX + body.len());
    framed.extend_from_slice(&len_buf);
    framed.extend_from_slice(&body);
    phux_protocol::wire::stream_bind::decode(&framed)
        .ok()
        .map(|(bind, _)| bind)
}

/// Fill `buf` from the stream: `Ok(true)` when filled, `Ok(false)` on a clean
/// finish before any byte (EOF at a boundary), `UnexpectedEof` on a partial
/// read, `Err` on a transport error.
async fn read_exact_quic(recv: &mut quinn::RecvStream, buf: &mut [u8]) -> io::Result<bool> {
    match recv.read_exact(buf).await {
        Ok(()) => Ok(true),
        Err(quinn::ReadExactError::FinishedEarly(0)) => Ok(false),
        Err(quinn::ReadExactError::FinishedEarly(_)) => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "QUIC stream finished during a framed value",
        )),
        Err(err) => Err(io::Error::other(err)),
    }
}

fn budget_closed(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotConnected,
        format!("{what} byte budget closed"),
    )
}

/// Read one frame while holding byte permits for it. Once the first header
/// byte arrives the rest must arrive within [`FRAME_BODY_DEADLINE`].
async fn read_framed_bounded(
    recv: &mut quinn::RecvStream,
    budget: &std::sync::Arc<tokio::sync::Semaphore>,
    terminal_budget: Option<&std::sync::Arc<tokio::sync::Semaphore>>,
    origin: Option<Arc<AtomicBool>>,
    diagnostics: Option<&crate::stream_diagnostics::StreamTracker>,
) -> io::Result<Option<AdmittedFrame>> {
    let mut header = [0u8; LENGTH_PREFIX];
    if !read_exact_quic(recv, &mut header[..1]).await? {
        return Ok(None);
    }
    let admitted = tokio::time::timeout(FRAME_BODY_DEADLINE, async {
        if !read_exact_quic(recv, &mut header[1..]).await? {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "QUIC stream finished mid-header",
            ));
        }
        let body_len = framing::decode_length(header)?;
        let permit_count = u32::try_from(LENGTH_PREFIX + body_len).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "QUIC frame size does not fit permit count",
            )
        })?;
        // Terminal sub-budget first: taking connection permits first would
        // let a second maximum Terminal frame eat the reserved control room.
        let terminal_bytes = match terminal_budget {
            Some(terminal_budget) => Some(
                terminal_budget
                    .clone()
                    .acquire_many_owned(permit_count)
                    .await
                    .map_err(|_| budget_closed("QUIC Terminal"))?,
            ),
            None => None,
        };
        let connection_bytes = budget
            .clone()
            .acquire_many_owned(permit_count)
            .await
            .map_err(|_| budget_closed("QUIC mux"))?;
        let mut framed = framing::frame_buffer(header)?;
        let ticket = diagnostics.map(|tracker| tracker.enqueue(u64::from(permit_count)));
        if !read_exact_quic(recv, &mut framed[LENGTH_PREFIX..]).await? {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stream finished mid-frame",
            ));
        }
        Ok::<_, io::Error>(AdmittedFrame {
            bytes: framed,
            _connection_bytes: Some(connection_bytes),
            _terminal_bytes: terminal_bytes,
            origin,
            event: None,
            _queue_ticket: ticket,
        })
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "QUIC incomplete-frame deadline elapsed",
        )
    })??;
    Ok(Some(admitted))
}

#[cfg(test)]
mod workload_adversarial;

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "tests hold stream events and permits to the end of each scripted step"
)]
mod tests {
    use super::*;

    use super::super::tls::{QUIC_ALPN, ensure_self_signed};
    use phux_protocol::ids::ResourceId;

    const TEST_TOKEN: [u8; crate::auth::TOKEN_LEN] = [0x11; crate::auth::TOKEN_LEN];
    /// One complete framed message: 4-byte length prefix (body = 3) + body.
    const FRAME: [u8; 7] = [0, 0, 0, 3, 0xde, 0xad, 0xbe];

    async fn join_bounded<S: Future, C: Future>(server: S, client: C) -> (S::Output, C::Output) {
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(server, client)
        })
        .await
        .expect("paired QUIC test completes")
    }

    fn focus_frame() -> BytesMut {
        let mut frame = BytesMut::new();
        phux_protocol::wire::frame::FrameKind::InputFocus {
            terminal_id: ResourceId::local(7),
            event: phux_protocol::input::focus::FocusEvent::Gained,
        }
        .encode(&mut frame);
        frame
    }

    /// A self-signed cert + key in a fresh tempdir, kept alive for the test.
    fn cert_pair() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        ensure_self_signed(&cert, &key).unwrap();
        (dir, cert, key)
    }

    /// A loopback listener; `tokens` requires a pairing-store preamble.
    fn listener(
        tokens: Option<Arc<crate::auth::ReloadingTokenStore>>,
    ) -> (tempfile::TempDir, QuicListener, SocketAddr) {
        let (dir, cert, key) = cert_pair();
        let listener = QuicListener::from_pem_with_client_ca_and_registry(
            "127.0.0.1:0".parse().unwrap(),
            &cert,
            &key,
            tokens,
            None,
            None,
        )
        .unwrap();
        let addr = listener.local_addr().unwrap();
        (dir, listener, addr)
    }

    /// A token store file holding the one known [`TEST_TOKEN`].
    fn token_store() -> (
        tempfile::NamedTempFile,
        Arc<crate::auth::ReloadingTokenStore>,
    ) {
        let file = tempfile::NamedTempFile::new().unwrap();
        crate::auth::write_test_credential(file.path(), &TEST_TOKEN);
        let store = crate::auth::ReloadingTokenStore::load(file.path().to_path_buf()).unwrap();
        (file, Arc::new(store))
    }

    /// A client endpoint offering the phux ALPN, trusting any certificate
    /// through the same builder production dialers use.
    fn client_endpoint_with(transport: Option<quinn::TransportConfig>) -> quinn::Endpoint {
        let crypto =
            phux_dial::tls::client_config(&phux_dial::CertTrust::SkipVerify, Some(QUIC_ALPN))
                .unwrap();
        let mut config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
        ));
        if let Some(transport) = transport {
            config.transport_config(Arc::new(transport));
        }
        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(config);
        endpoint
    }

    fn client_endpoint() -> quinn::Endpoint {
        client_endpoint_with(None)
    }

    /// Connect to `addr` and open the control stream.
    async fn open_control(
        endpoint: &quinn::Endpoint,
        addr: SocketAddr,
    ) -> (quinn::Connection, quinn::SendStream, quinn::RecvStream) {
        let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
        let (send, recv) = conn.open_bi().await.unwrap();
        (conn, send, recv)
    }

    /// Frame a token as the auth preamble: `len: u32 BE` + token bytes.
    fn token_preamble(token: &[u8]) -> Vec<u8> {
        let mut buf = (u32::try_from(token.len()).unwrap()).to_be_bytes().to_vec();
        buf.extend_from_slice(token);
        buf
    }

    fn stream_bind_bytes(terminal: u32, stream: u64) -> Vec<u8> {
        use phux_protocol::ids::StreamId;
        use phux_protocol::wire::stream_bind::{StreamBind, encode};
        let mut buf = BytesMut::new();
        encode(
            &StreamBind {
                terminal_id: ResourceId::local(terminal),
                stream_id: StreamId::new(stream).unwrap(),
            },
            &mut buf,
        );
        buf.to_vec()
    }

    /// Start the admitted pump for a `Bound` event, as the client task does.
    /// Returns the bound ids, the Terminal byte budget, and the liveness flag.
    fn spawn_pump(
        event: QuicStreamEvent,
    ) -> (
        ResourceId,
        phux_protocol::ids::StreamId,
        Arc<tokio::sync::Semaphore>,
        Arc<AtomicBool>,
    ) {
        let QuicStreamEvent::Bound {
            terminal_id,
            stream_id,
            recv,
            frames,
            frame_bytes,
            terminal_frame_bytes,
            ..
        } = event
        else {
            panic!("expected Bound, got {event:?}");
        };
        let active = Arc::new(AtomicBool::new(true));
        tokio::spawn(pump_terminal_stream(
            recv,
            terminal_id.clone(),
            stream_id,
            frames,
            Arc::clone(&frame_bytes),
            Arc::clone(&terminal_frame_bytes),
            Arc::clone(&active),
            tokio_util::sync::CancellationToken::new(),
            None,
        ));
        (terminal_id, stream_id, terminal_frame_bytes, active)
    }

    #[tokio::test]
    async fn partial_frames_are_typed_as_truncation() {
        for bytes in [&[0, 0][..], &[0, 0, 0, 3, 0xde]] {
            let (_dir, listener, addr) = listener(None);
            let server = async {
                let (mut reader, _writer, _) = listener.accept().await.unwrap();
                let err = reader.read_frame().await.expect_err("truncation");
                assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "{bytes:?}");
            };
            let client = async {
                let (_conn, mut send, _recv) = open_control(&client_endpoint(), addr).await;
                send.write_all(bytes).await.unwrap();
                send.finish().unwrap();
                tokio::time::sleep(Duration::from_millis(200)).await;
            };
            join_bounded(server, client).await;
        }
    }

    /// Loopback admits without a preamble and stamps no device id; a paired
    /// listener authenticates the preamble and stamps one.
    #[tokio::test]
    async fn round_trips_a_frame_open_and_authenticated() {
        let (_tokens, store) = token_store();
        for tokens in [None, Some(store)] {
            let authenticated = tokens.is_some();
            let (_dir, listener, addr) = listener(tokens);
            let server = async {
                let (mut reader, _writer, peer) = listener.accept().await.unwrap();
                (reader.read_frame().await.unwrap(), peer)
            };
            let client = async {
                let (_conn, mut send, _recv) = open_control(&client_endpoint(), addr).await;
                if authenticated {
                    send.write_all(&token_preamble(&TEST_TOKEN)).await.unwrap();
                }
                send.write_all(&FRAME).await.unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
            };
            let ((got, peer), ()) = join_bounded(server, client).await;
            assert_eq!(got.unwrap().as_ref(), &FRAME);
            assert_eq!(peer.transport, TransportType::Quic);
            assert_eq!(peer.mcp_host_key.is_some(), authenticated);
        }
    }

    /// A peer that completes the handshake and then says nothing holds one
    /// admission slot, not the listener: a healthy peer behind it is
    /// admitted at once instead of after the silent one's deadline.
    #[tokio::test]
    async fn a_silent_connection_does_not_delay_other_admissions() {
        let (_tokens, store) = token_store();
        let (_dir, listener, addr) = listener(Some(store));
        let silent_endpoint = client_endpoint();
        let healthy_endpoint = client_endpoint();
        let server = async {
            let (mut reader, _writer, _peer) = listener.accept().await.unwrap();
            reader.read_frame().await.unwrap()
        };
        let client = async {
            let silent = silent_endpoint
                .connect(addr, "localhost")
                .unwrap()
                .await
                .unwrap();
            let (_conn, mut send, _recv) = open_control(&healthy_endpoint, addr).await;
            send.write_all(&token_preamble(&TEST_TOKEN)).await.unwrap();
            send.write_all(&FRAME).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            silent
        };
        let (got, _silent) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(server, client)
        })
        .await
        .expect("the healthy peer is admitted without waiting on the silent one");
        assert_eq!(got.unwrap().as_ref(), &FRAME);
    }

    #[tokio::test]
    async fn invalid_token_preamble_is_refused() {
        let (_tokens, store) = token_store();
        let (_dir, listener, addr) = listener(Some(store));
        // The listener loops on refusals, so assert from the client side
        // while it runs.
        let client = async {
            let (conn, mut send, _recv) = open_control(&client_endpoint(), addr).await;
            let _ = send
                .write_all(&token_preamble(&[0x22u8; crate::auth::TOKEN_LEN]))
                .await;
            tokio::time::timeout(Duration::from_secs(5), conn.closed())
                .await
                .expect("server must refuse promptly, not hang")
        };
        let closed = tokio::select! {
            closed = client => closed,
            _ = listener.accept() => panic!("a refused peer was admitted"),
        };
        match closed {
            quinn::ConnectionError::ApplicationClosed(close) => {
                assert_eq!(u64::from(AUTH_FAILED_CODE), close.error_code.into_inner());
            }
            other => panic!("expected application close on auth failure, got {other:?}"),
        }
    }

    /// A log sink for one test thread's WARN lines.
    #[derive(Clone, Default)]
    struct CapturedLog(Arc<std::sync::Mutex<Vec<u8>>>);

    impl io::Write for CapturedLog {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLog {
        type Writer = Self;
        fn make_writer(&'a self) -> Self {
            self.clone()
        }
    }

    impl CapturedLog {
        fn text(&self) -> String {
            String::from_utf8_lossy(
                &self
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            )
            .into_owned()
        }
    }

    /// An unauthenticated peer cannot flood the log: refused preambles warn
    /// once per interval, and the rest are counted into the next warning.
    #[tokio::test]
    async fn refused_preambles_warn_at_most_once_per_interval() {
        let log = CapturedLog::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(log.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let (_tokens, store) = token_store();
        let (_dir, listener, addr) = listener(Some(store));
        let clients = async {
            for _ in 0..4 {
                let (conn, mut send, _recv) = open_control(&client_endpoint(), addr).await;
                let _ = send
                    .write_all(&token_preamble(&[0x22u8; crate::auth::TOKEN_LEN]))
                    .await;
                tokio::time::timeout(Duration::from_secs(5), conn.closed())
                    .await
                    .expect("server must refuse promptly");
            }
        };
        tokio::select! {
            () = clients => {}
            _ = listener.accept() => panic!("a refused peer was admitted"),
        }
        let warnings = log.text().matches("quic consumer refused").count();
        assert_eq!(warnings, 1, "{}", log.text());
    }

    /// A CSR-enrolled workload client: its identity files and credential.
    fn enrolled_client(
        paths: &crate::workload::WorkloadPaths,
        dir: &std::path::Path,
    ) -> (
        phux_dial::TlsClientIdentity,
        crate::workload::RegisteredCredential,
    ) {
        let client_key = rcgen::KeyPair::generate().unwrap();
        let csr = rcgen::CertificateParams::new(vec!["client".to_owned()])
            .unwrap()
            .serialize_request(&client_key)
            .unwrap();
        let material =
            crate::workload::ClientMaterial::from_pem(csr.pem().unwrap().as_bytes()).unwrap();
        let expires = chrono::Utc::now().timestamp() + 3600;
        let prepared = crate::workload::prepare_enrollment(paths, &material, expires).unwrap();
        let certificate = dir.join("client.pem");
        let private_key = dir.join("client.key");
        std::fs::write(&certificate, prepared.issued_chain_pem().unwrap()).unwrap();
        std::fs::write(&private_key, client_key.serialize_pem()).unwrap();
        let registered = prepared
            .commit(&paths.registry, vec!["*@global".to_owned()], expires)
            .unwrap();
        (
            phux_dial::TlsClientIdentity::PemFiles {
                certificate,
                private_key,
            },
            registered,
        )
    }

    /// The `OPEN_LISTENER` door in workload mode keeps its one-attach token
    /// as outer admission, refuses a dial without a client certificate, and
    /// stamps an enrolled certificate's credential.
    #[tokio::test]
    async fn an_ephemeral_listener_in_workload_mode_requires_an_enrolled_certificate() {
        let (dir, cert, key) = cert_pair();
        let paths = crate::workload::WorkloadPaths {
            ca_cert: dir.path().join("ca.pem"),
            ca_key: dir.path().join("ca.key"),
            registry: dir.path().join("workload-keys"),
        };
        crate::workload::init_authority(&paths.ca_cert, &paths.ca_key).unwrap();
        let (identity, enrolled) = enrolled_client(&paths, dir.path());
        let ca_certificate = crate::workload::authority_certificate(&paths.ca_cert).unwrap();
        let (token, secret) = crate::auth::ListenerToken::mint().unwrap();
        let token = Arc::new(token);
        let door = || {
            let registry = Arc::new(
                crate::workload::ReloadingWorkloadRegistry::load(paths.registry.clone()).unwrap(),
            );
            QuicListener::with_admission(
                "127.0.0.1:0".parse().unwrap(),
                &cert,
                &key,
                QuicAdmission::Listener(Arc::clone(&token)),
                Some((&ca_certificate, registry)),
            )
            .unwrap()
        };

        let refusing = door();
        let refusing_addr = refusing.local_addr().unwrap();
        let anonymous = client_endpoint();
        let refused = async {
            let conn = anonymous
                .connect(refusing_addr, "localhost")
                .ok()?
                .await
                .ok()?;
            let (mut send, mut recv) = conn.open_bi().await.ok()?;
            send.write_all(&token_preamble(&secret)).await.ok()?;
            let mut byte = [0u8; 1];
            recv.read_exact(&mut byte).await.ok()
        };
        let refused = tokio::select! {
            refused = tokio::time::timeout(Duration::from_secs(10), refused) => refused,
            _ = refusing.accept() => panic!("a dial without a certificate was admitted"),
        };
        assert!(
            refused.expect("a refusal settles").is_none(),
            "a dial without a workload certificate is refused"
        );

        let listener = door();
        let addr = listener.local_addr().unwrap();
        let crypto = phux_dial::tls::client_config_with_identity(
            &phux_dial::CertTrust::SkipVerify,
            &identity,
            Some(QUIC_ALPN),
        )
        .unwrap();
        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
        )));
        let server = async {
            let (mut reader, _writer, peer) = listener.accept().await.unwrap();
            (reader.read_frame().await.unwrap(), peer)
        };
        let client = async {
            let (_conn, mut send, _recv) = open_control(&endpoint, addr).await;
            send.write_all(&token_preamble(&secret)).await.unwrap();
            send.write_all(&FRAME).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let ((got, peer), ()) = join_bounded(server, client).await;
        assert_eq!(got.unwrap().as_ref(), &FRAME);
        let credential = peer.credential.as_ref().expect("the workload credential");
        assert_eq!(credential.id, enrolled.id);
        assert_eq!(credential.generation, enrolled.generation);
        assert!(credential.registry_instance.is_some());
    }

    #[test]
    fn terminal_stream_provenance_rejects_cross_target_and_control_frames() {
        use phux_protocol::ids::{BootstrapId, StreamId};
        use phux_protocol::wire::frame::FrameKind;

        let bound = ResourceId::local(7);
        let stream = StreamId::new(3).unwrap();
        let mut encoded = BytesMut::new();
        FrameKind::HistoryRequest {
            terminal_id: bound.clone(),
            stream_id: stream,
            bootstrap_id: BootstrapId::new(9).unwrap(),
            cursor: bytes::Bytes::new(),
            max_bytes: 1,
            max_rows: 1,
        }
        .encode(&mut encoded);
        assert!(terminal_frame_matches(&encoded, &bound, stream));
        assert!(terminal_frame_matches(&focus_frame(), &bound, stream));
        assert!(!terminal_frame_matches(
            &focus_frame(),
            &ResourceId::local(8),
            stream
        ));
        encoded.clear();
        FrameKind::Ping { nonce: 1 }.encode(&mut encoded);
        assert!(!terminal_frame_matches(&encoded, &bound, stream));
    }

    /// A loopback connection whose control stream already carried [`FRAME`].
    struct Connected {
        _dir: tempfile::TempDir,
        _listener: QuicListener,
        _endpoint: quinn::Endpoint,
        conn: quinn::Connection,
        control: quinn::SendStream,
        control_recv: quinn::RecvStream,
        reader: QuicMuxReader,
        writer: QuicWriter,
    }

    async fn connected(endpoint: quinn::Endpoint) -> Connected {
        let (dir, listener, addr) = listener(None);
        let client = async {
            let (conn, mut send, recv) = open_control(&endpoint, addr).await;
            send.write_all(&FRAME).await.unwrap();
            (conn, send, recv)
        };
        let (accepted, (conn, control, control_recv)) =
            join_bounded(listener.accept(), client).await;
        let (reader, writer, _) = accepted.unwrap();
        Connected {
            _dir: dir,
            _listener: listener,
            _endpoint: endpoint,
            conn,
            control,
            control_recv,
            reader,
            writer,
        }
    }

    impl Connected {
        /// Read the control frame, then upgrade the mux (once only).
        async fn upgrade(&mut self) -> tokio::sync::mpsc::Receiver<QuicStreamEvent> {
            let first = self.reader.read_frame().await.unwrap().unwrap();
            assert_eq!(first.as_ref(), &FRAME);
            let events = self.reader.take_stream_events().expect("upgrade once");
            assert!(self.reader.take_stream_events().is_none(), "one-shot");
            events
        }

        /// Open a Terminal stream and send its `STREAM_BIND`.
        async fn bind(&self, terminal: u32) -> (quinn::SendStream, quinn::RecvStream) {
            let (mut send, recv) = self.conn.open_bi().await.unwrap();
            send.write_all(&stream_bind_bytes(terminal, 1))
                .await
                .unwrap();
            (send, recv)
        }

        async fn read_frame(&mut self) -> BytesMut {
            tokio::time::timeout(Duration::from_secs(5), self.reader.read_frame())
                .await
                .expect("frame arrives")
                .unwrap()
                .unwrap()
        }

        /// The next lifecycle event; a frame in the meantime is a failure.
        async fn next_event_not_frame(
            &mut self,
            events: &mut tokio::sync::mpsc::Receiver<QuicStreamEvent>,
        ) -> QuicStreamEvent {
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    event = events.recv() => event.expect("event channel open"),
                    frame = self.reader.read_frame() => panic!("unexpected frame: {frame:?}"),
                }
            })
            .await
            .expect("event arrives")
        }
    }

    async fn next_event(
        events: &mut tokio::sync::mpsc::Receiver<QuicStreamEvent>,
    ) -> QuicStreamEvent {
        tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("event arrives")
            .expect("event channel open")
    }

    #[tokio::test]
    async fn mux_upgrades_and_merges_terminal_streams() {
        let mut c = connected(client_endpoint()).await;
        let mut events = c.upgrade().await;
        let (mut term, _term_recv) = c.bind(7).await;
        let (terminal_id, stream_id, _, _) = spawn_pump(next_event(&mut events).await);
        assert_eq!((terminal_id, stream_id.get()), (ResourceId::local(7), 1));
        term.write_all(&focus_frame()).await.unwrap();
        assert_eq!(c.read_frame().await, focus_frame());
        // A clean client finish surfaces as the detach signal.
        term.finish().unwrap();
        let ended = c.next_event_not_frame(&mut events).await;
        assert!(
            matches!(&ended, QuicStreamEvent::Ended { terminal_id, stream_id }
                if *terminal_id == ResourceId::local(7) && stream_id.get() == 1),
            "{ended:?}"
        );
    }

    /// A cancelled `read_frame` must not lose a dequeued lifecycle event.
    #[tokio::test]
    async fn mux_keeps_an_end_event_when_read_is_cancelled() {
        let mut c = connected(client_endpoint()).await;
        let (frames_tx, frames_rx) = tokio::sync::mpsc::channel(2);
        let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(1);
        let ended = |id| QuicStreamEvent::Ended {
            terminal_id: ResourceId::local(id),
            stream_id: phux_protocol::ids::StreamId::new(1).unwrap(),
        };
        events_tx.try_send(ended(1)).unwrap();
        frames_tx.try_send(AdmittedFrame::event(ended(2))).unwrap();
        let mut frame = AdmittedFrame::event(ended(3));
        frame.event = None;
        frame.bytes = BytesMut::from(FRAME.as_slice());
        frames_tx.try_send(frame).unwrap();
        c.reader.frames_rx = Some(frames_rx);
        c.reader.stream_events_tx = Some(events_tx);
        {
            let read = c.reader.read_frame();
            tokio::pin!(read);
            std::future::poll_fn(|cx| {
                assert!(std::future::Future::poll(read.as_mut(), cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
        }
        let is_end = |event: Option<QuicStreamEvent>, id| {
            matches!(event, Some(QuicStreamEvent::Ended { terminal_id, .. })
                if terminal_id == ResourceId::local(id))
        };
        assert!(is_end(events_rx.recv().await, 1));
        assert_eq!(c.read_frame().await.as_ref(), &FRAME);
        assert!(is_end(events_rx.try_recv().ok(), 2));
    }

    #[tokio::test]
    async fn cancelled_partial_writer_resets_instead_of_finishing() {
        let mut transport = quinn::TransportConfig::default();
        transport.stream_receive_window(32_768_u32.into());
        let mut c = connected(client_endpoint_with(Some(transport))).await;
        let context = c.writer.diagnostic_tracker().context();
        let frame = vec![0_u8; 65_536];
        {
            let write = c.writer.write_frame(&frame);
            tokio::pin!(write);
            assert!(
                tokio::time::timeout(Duration::from_millis(100), &mut write)
                    .await
                    .is_err(),
                "stream credit must hold the partial write"
            );
            let sample = crate::stream_diagnostics::snapshot()
                .streams
                .into_iter()
                .find(|sample| sample.context == context)
                .expect("registered writer");
            assert!(sample.write_in_progress_age_us.unwrap() >= 90_000);
        }
        drop(c.writer);
        assert!(
            !crate::stream_diagnostics::snapshot()
                .streams
                .iter()
                .any(|sample| sample.context == context),
            "cancelled writer registration removed"
        );
        let error = c.control_recv.read_to_end(65_536).await.unwrap_err();
        assert!(
            matches!(error, quinn::ReadToEndError::Read(quinn::ReadError::Reset(code))
                if code == quinn::VarInt::from_u32(0x10)),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn mux_diagnostics_account_admitted_bytes_until_delivery() {
        let mut c = connected(client_endpoint()).await;
        let context = c.writer.diagnostic_tracker().context();
        let sample = || {
            crate::stream_diagnostics::snapshot()
                .streams
                .into_iter()
                .find(|sample| sample.context == context)
                .unwrap()
        };
        let _events = c.upgrade().await;
        c.control.write_all(&FRAME).await.unwrap();
        while c.reader.frames_rx.as_ref().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
        let queued = sample();
        assert!(queued.active);
        assert_eq!(queued.queue_bytes, FRAME.len() as u64);
        assert_eq!(queued.queue_items, 1);
        assert!(queued.queue_oldest_age_us.is_some());
        assert_eq!(c.read_frame().await.as_ref(), &FRAME);
        let drained = sample();
        assert_eq!(drained.queue_bytes, 0);
        assert_eq!(drained.queue_oldest_age_us, None);
    }

    #[tokio::test]
    async fn mux_resets_a_stream_with_no_well_formed_bind() {
        let mut c = connected(client_endpoint()).await;
        let mut events = c.upgrade().await;
        let (mut bad_send, mut bad_recv) = c.conn.open_bi().await.unwrap();
        bad_send.write_all(b"not a bind header").await.unwrap();
        let mut buf = [0u8; 8];
        let reset = tokio::time::timeout(Duration::from_secs(5), bad_recv.read(&mut buf)).await;
        assert!(
            matches!(reset, Ok(Err(_))),
            "reset stream errors, got {reset:?}"
        );
        // The malformed stream was reset first, so the first event must be
        // the valid bind.
        let _valid = c.bind(7).await;
        let event = next_event(&mut events).await;
        assert!(
            matches!(&event, QuicStreamEvent::Bound { terminal_id, stream_id, .. }
                if *terminal_id == ResourceId::local(7) && stream_id.get() == 1),
            "malformed bind emitted an event: {event:?}"
        );
    }

    #[tokio::test]
    async fn dropping_mux_aborts_incomplete_bind_workers() {
        let mut c = connected(client_endpoint()).await;
        let mut events = c.upgrade().await;
        let (mut incomplete, _recv) = c.conn.open_bi().await.unwrap();
        incomplete.write_all(&[0]).await.unwrap();
        let sender = c.reader.stream_events_tx.clone().expect("mux event sender");
        tokio::time::timeout(Duration::from_secs(5), async {
            // Ours, the reader's, and the incomplete bind worker's.
            while sender.strong_count() < 4 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("incomplete bind worker starts");
        drop(sender);
        drop(c.reader);
        let closed = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("bind worker ownership closes event channel");
        assert!(closed.is_none());
    }

    #[tokio::test]
    async fn mux_preserves_typed_terminal_framing_failure() {
        let mut c = connected(client_endpoint()).await;
        let mut events = c.upgrade().await;
        let (mut terminal, _recv) = c.bind(7).await;
        spawn_pump(next_event(&mut events).await);
        terminal.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
        let event = c.next_event_not_frame(&mut events).await;
        assert!(matches!(
            event,
            QuicStreamEvent::Failed {
                failure: QuicStreamFailure::Framing(framing::FramingError::LengthOutOfRange {
                    length: u32::MAX
                }),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn retired_terminal_frames_cannot_survive_into_shared_dispatch() {
        let mut c = connected(client_endpoint()).await;
        let mut events = c.upgrade().await;
        let (mut terminal, _recv) = c.bind(7).await;
        let (_, _, _, active) = spawn_pump(next_event(&mut events).await);
        terminal.write_all(&focus_frame()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while c.reader.frames_rx.as_ref().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("terminal frame reaches shared dispatch");
        active.store(false, Ordering::Release);
        c.control.write_all(&FRAME).await.unwrap();
        assert_eq!(
            c.read_frame().await.as_ref(),
            &FRAME,
            "retired Terminal frame was discarded"
        );
    }

    #[tokio::test]
    async fn incomplete_terminal_bodies_cannot_starve_control() {
        let mut c = connected(client_endpoint()).await;
        let mut events = c.upgrade().await;
        let (mut first, _first_recv) = c.bind(7).await;
        let (mut second, _second_recv) = c.bind(8).await;
        let (_, _, terminal_budget, _) = spawn_pump(next_event(&mut events).await);
        spawn_pump(next_event(&mut events).await);
        let max_header = phux_protocol::wire::frame::MAX_FRAME_LEN.to_be_bytes();
        first.write_all(&max_header).await.unwrap();
        second.write_all(&max_header).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while terminal_budget.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("terminal pump reserves its incomplete body");
        c.control.write_all(&FRAME).await.unwrap();
        assert_eq!(
            c.read_frame().await.as_ref(),
            &FRAME,
            "control is not starved"
        );
    }
}
