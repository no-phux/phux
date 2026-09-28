//! A negotiated connection to a phux server over UDS, QUIC, or WebSocket,
//! with SPEC §5 framing (owned by [`phux_protocol::wire::framing`]) and the
//! correlated request/response primitives every verb uses.

use std::io;
use std::path::{Path, PathBuf};

use bytes::BytesMut;
use phux_client_core::handshake::validate_hello_ok;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ClientCapabilities, Layer, LayerSet, ServerFeature,
    ServerFeatureSet,
};
use phux_protocol::ids::{ResourceId, StreamId};
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, MoveResult, Scope, SpawnResult,
};
use phux_protocol::wire::framing;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use super::outcome::AttachError;
use super::quic;
pub use super::quic::{CertTrust, QuicDial};
use super::ws;
pub use super::ws::WsDial;

/// How to reach a server; reconnects dial the same way each attempt.
#[derive(Debug, Clone)]
pub enum Dial {
    /// Connect over the Unix domain socket at this path.
    Uds(PathBuf),
    /// Dial a remote QUIC listener.
    Quic(QuicDial),
    /// Dial a remote WebSocket listener.
    Ws(WsDial),
}

impl Dial {
    /// A [`Dial::Uds`] for `path`.
    #[must_use]
    pub fn uds(path: &Path) -> Self {
        Self::Uds(path.to_path_buf())
    }
}

/// A connection that has completed `HELLO` negotiation.
///
/// # The COMMAND interleave contract
///
/// L1 §5 lets the server emit other frames before a `COMMAND_RESULT`, and it
/// does so on paths a client cannot avoid: `ATTACH_RESOURCE` pushes the
/// bootstrap transcript before its ack, a federation hub pushes one
/// uncorrelated `ERROR` per unreachable satellite before a `GET_STATE`
/// reply, and any subscription on the connection can fan events in first.
/// Those frames are never re-sent, so use [`Connection::request`] (and its
/// typed siblings), which return them with the answer. Raw
/// [`Connection::send`] + [`Connection::recv`] is only for a full-duplex loop
/// that routes every frame kind.
#[derive(Debug)]
pub struct Connection {
    reader: FrameReader,
    writer: FrameWriter,
    /// Peer pid from the UDS peer credentials; `None` on remote transports.
    peer_pid: Option<i32>,
    /// What `HELLO_OK` selected. `None` only on the `from_stream` test seam.
    negotiated_bootstrap: Option<NegotiatedBootstrap>,
    /// `HELLO_OK.server_id`, the server-incarnation id (ADR-0053) the input
    /// replay journal compares across reconnects.
    server_id: Option<Vec<u8>>,
    next_attach_id: u32,
    /// Per-Terminal QUIC stream state (proto.md §4.2); QUIC only.
    multistream: Option<Multistream>,
}

/// QUIC multi-stream state (proto.md §4.2, ADR-0115): each bound Terminal
/// has its own stream, so per-stream backpressure never stalls another pane
/// or the control channel. Their frames merge into one channel that
/// [`Connection::recv`] reads beside the control stream.
#[derive(Debug)]
struct Multistream {
    /// The connection the Terminal streams ride.
    conn: quinn::Connection,
    /// Live Terminal streams by Terminal.
    bindings: std::collections::HashMap<ResourceId, MuxBinding>,
    /// Next app-level `StreamId`: monotonic, never zero; a re-bind gets a new
    /// one (a new generation, L1 §4.6).
    next_stream_id: u64,
    /// Merged frames from every bound Terminal stream's pump task.
    frames_rx: tokio::sync::mpsc::Receiver<MuxItem>,
    /// Sender half the pump tasks hold.
    frames_tx: tokio::sync::mpsc::Sender<MuxItem>,
    /// Connection-wide queued/incomplete Terminal-stream frame byte budget.
    frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
    /// Terminals whose stream the server ended. The end races
    /// `RESOURCE_CLOSED` on control (L1 §4.9), so frames still addressed to
    /// one are dropped rather than failed.
    ended: std::collections::HashSet<ResourceId>,
    #[cfg(feature = "testkit")]
    /// Maximum time from a Terminal frame's first byte to its complete body.
    terminal_frame_deadline: std::time::Duration,
}

/// One bound Terminal stream's client-side state.
#[derive(Debug)]
struct MuxBinding {
    /// The app-level `StreamId` this stream opened under.
    stream_id: StreamId,
    /// This stream's send half; Terminal-addressed frames route here.
    send: quinn::SendStream,
    /// Receive pump for this generation; aborted on every local teardown.
    receive_task: tokio::task::AbortHandle,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl MuxBinding {
    fn retire(&mut self) {
        self.active
            .store(false, std::sync::atomic::Ordering::Release);
        self.receive_task.abort();
        let _ = self.send.finish();
    }
}

#[derive(Debug)]
struct MuxFrame {
    terminal_id: ResourceId,
    stream_id: StreamId,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    bytes: Result<BytesMut, String>,
    _bytes: Option<tokio::sync::OwnedSemaphorePermit>,
}

#[derive(Debug)]
enum MuxItem {
    Frame(MuxFrame),
    End {
        terminal_id: ResourceId,
        stream_id: StreamId,
        active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    },
}

/// Depth of the merged frame channel; a stalled reader backpressures into
/// QUIC flow control, not loss.
const MUX_FRAME_CHANNEL: usize = 64;
const MUX_FRAME_BYTES: usize = 32 * 1024 * 1024;
const MAX_CLIENT_TERMINAL_STREAMS: usize = 127;
const TERMINAL_STREAM_OPEN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
const TERMINAL_FRAME_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

impl Multistream {
    fn new(conn: quinn::Connection) -> Self {
        let (frames_tx, frames_rx) = tokio::sync::mpsc::channel(MUX_FRAME_CHANNEL);
        Self {
            conn,
            bindings: std::collections::HashMap::new(),
            next_stream_id: 1,
            frames_rx,
            frames_tx,
            frame_bytes: std::sync::Arc::new(tokio::sync::Semaphore::new(MUX_FRAME_BYTES)),
            ended: std::collections::HashSet::new(),
            #[cfg(feature = "testkit")]
            terminal_frame_deadline: TERMINAL_FRAME_DEADLINE,
        }
    }

    /// Retire the binding a server-side stream end names, if it is still the
    /// current one; a stale end from a replaced generation is ignored.
    fn end_stream(
        &mut self,
        terminal_id: ResourceId,
        stream_id: StreamId,
        active: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        if !mux_origin_is_current(self, &terminal_id, stream_id, active) {
            return;
        }
        if let Some(mut binding) = self.bindings.remove(&terminal_id) {
            binding.retire();
            self.ended.insert(terminal_id);
        }
    }

    #[cfg(feature = "testkit")]
    const fn set_terminal_frame_deadline_for_test(&mut self, deadline: std::time::Duration) {
        self.terminal_frame_deadline = deadline;
    }

    #[cfg_attr(not(feature = "testkit"), allow(clippy::unused_self))]
    const fn terminal_frame_deadline(&self) -> std::time::Duration {
        #[cfg(feature = "testkit")]
        {
            self.terminal_frame_deadline
        }
        #[cfg(not(feature = "testkit"))]
        {
            TERMINAL_FRAME_DEADLINE
        }
    }

    const fn next_stream_id(&mut self) -> Option<StreamId> {
        let raw = self.next_stream_id;
        self.next_stream_id = self.next_stream_id.wrapping_add(1);
        if self.next_stream_id == 0 {
            self.next_stream_id = 1;
        }
        StreamId::new(raw)
    }
}

/// Pump complete frames from one Terminal stream into the merged queue.
async fn pump_terminal_stream(
    mut recv: quinn::RecvStream,
    terminal_id: ResourceId,
    stream_id: StreamId,
    frames_tx: tokio::sync::mpsc::Sender<MuxItem>,
    frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    terminal_frame_deadline: std::time::Duration,
) {
    loop {
        let mut header = [0_u8; framing::LENGTH_PREFIX_LEN];
        match recv.read_exact(&mut header[..1]).await {
            Ok(()) => {}
            Err(quinn::ReadExactError::FinishedEarly(0)) => break,
            Err(error) => {
                send_mux_error(
                    &frames_tx,
                    &terminal_id,
                    stream_id,
                    &active,
                    format!("Terminal stream truncated before frame header: {error}"),
                )
                .await;
                break;
            }
        }
        let read = tokio::time::timeout(terminal_frame_deadline, async {
            recv.read_exact(&mut header[1..])
                .await
                .map_err(|error| error.to_string())?;
            let body_len = framing::decode_length(header).map_err(|error| error.to_string())?;
            let total = framing::LENGTH_PREFIX_LEN + body_len;
            let permits = u32::try_from(total).map_err(|error| error.to_string())?;
            let bytes = frame_bytes
                .clone()
                .acquire_many_owned(permits)
                .await
                .map_err(|error| format!("Terminal stream byte budget closed: {error}"))?;
            let mut frame = framing::frame_buffer(header).map_err(|error| error.to_string())?;
            recv.read_exact(&mut frame[framing::LENGTH_PREFIX_LEN..])
                .await
                .map_err(|error| format!("Terminal stream finished mid-frame: {error}"))?;
            Ok::<_, String>((frame, bytes))
        })
        .await
        .map_err(|_| "Terminal stream incomplete-frame deadline elapsed".to_owned())
        .and_then(|result| result);
        let (frame, bytes) = match read {
            Ok(frame) => frame,
            Err(error) => {
                send_mux_error(&frames_tx, &terminal_id, stream_id, &active, error).await;
                break;
            }
        };
        let item = MuxFrame {
            terminal_id: terminal_id.clone(),
            stream_id,
            active: std::sync::Arc::clone(&active),
            bytes: Ok(frame),
            _bytes: Some(bytes),
        };
        if frames_tx.send(MuxItem::Frame(item)).await.is_err() {
            return;
        }
    }
    let _ = frames_tx
        .send(MuxItem::End {
            terminal_id,
            stream_id,
            active,
        })
        .await;
}

async fn send_mux_error(
    frames_tx: &tokio::sync::mpsc::Sender<MuxItem>,
    terminal_id: &ResourceId,
    stream_id: StreamId,
    active: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    error: String,
) {
    let _ = frames_tx
        .send(MuxItem::Frame(MuxFrame {
            terminal_id: terminal_id.clone(),
            stream_id,
            active: std::sync::Arc::clone(active),
            bytes: Err(error),
            _bytes: None,
        }))
        .await;
}

/// The Terminal whose dedicated stream `frame` routes over, if bound.
const fn terminal_target(frame: &FrameKind) -> Option<&ResourceId> {
    match frame {
        FrameKind::InputKey { terminal_id, .. }
        | FrameKind::InputMouse { terminal_id, .. }
        | FrameKind::InputFocus { terminal_id, .. }
        | FrameKind::InputPaste { terminal_id, .. }
        | FrameKind::InputTerminalReply { terminal_id, .. }
        | FrameKind::FrameAck { terminal_id, .. }
        | FrameKind::HistoryRequest { terminal_id, .. }
        | FrameKind::ResizeTerminal { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}

fn mux_frame_is_current(mux: &Multistream, frame: &MuxFrame) -> bool {
    mux_origin_is_current(mux, &frame.terminal_id, frame.stream_id, &frame.active)
}

fn mux_origin_is_current(
    mux: &Multistream,
    terminal_id: &ResourceId,
    stream_id: StreamId,
    active: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> bool {
    active.load(std::sync::atomic::Ordering::Acquire)
        && mux.bindings.get(terminal_id).is_some_and(|binding| {
            binding.stream_id == stream_id && std::sync::Arc::ptr_eq(&binding.active, active)
        })
}

async fn send_quic_frame(
    send: &mut quinn::SendStream,
    frame: &FrameKind,
) -> Result<(), AttachError> {
    let mut out = BytesMut::with_capacity(256);
    frame.encode(&mut out);
    send.write_all(&out)
        .await
        .map_err(|err| AttachError::Io(io::Error::other(err)))
}

/// Immutable result of protocol-0.7 bootstrap negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NegotiatedBootstrap {
    /// Exact synchronization profile selected by the server.
    pub profile: BootstrapProfile,
    /// Exact per-frame bootstrap/history payload bounds.
    pub limits: BootstrapLimits,
    /// Additive server features authenticated by `HELLO_OK`.
    pub server_features: ServerFeatureSet,
}

#[derive(Debug)]
enum FrameReader {
    Uds(UdsReader),
    Quic(QuicReader),
    Ws(WsReader),
}

#[derive(Debug)]
enum FrameWriter {
    Uds(UdsWriter),
    Quic(QuicWriter),
    Ws(WsWriter),
}

/// UDS read half. Reads in chunks so one syscall can surface several queued
/// frames, which `try_recv` drains so a burst coalesces into one paint.
#[derive(Debug)]
struct UdsReader {
    inner: OwnedReadHalf,
    buf: BytesMut,
    /// Bounds from `HELLO_OK` (the client's own while it is decoded).
    bootstrap_limits: BootstrapLimits,
}

#[derive(Debug)]
struct UdsWriter {
    inner: OwnedWriteHalf,
    /// Encode buffer: one frame, or the whole batch while `corked`.
    out: BytesMut,
    /// Held only across dispatch of input already read, so it batches a
    /// multi-event read into one write and never delays anything.
    corked: bool,
}

/// QUIC read half: the same chunked reassembly as [`UdsReader`]. Holds the
/// endpoint and connection so the I/O driver outlives the stream.
#[derive(Debug)]
struct QuicReader {
    recv: quinn::RecvStream,
    buf: BytesMut,
    /// Landing pad for [`Self::poll_read_once`], zero-initialized once.
    scratch: Box<[u8]>,
    _endpoint: quinn::Endpoint,
    _connection: quinn::Connection,
    bootstrap_limits: BootstrapLimits,
}

/// Room offered per non-blocking QUIC top-up: a full coalesced server write.
const QUIC_TRY_READ_BYTES: usize = 64 * 1024;

/// QUIC write half; its [`Drop`] issues a best-effort `CONNECTION_CLOSE`.
#[derive(Debug)]
struct QuicWriter {
    send: quinn::SendStream,
    out: BytesMut,
    endpoint: quinn::Endpoint,
    connection: quinn::Connection,
}

/// WebSocket read half: one binary message is one encoded frame.
#[derive(Debug)]
struct WsReader {
    inner: ws::WsReader,
    bootstrap_limits: BootstrapLimits,
}

#[derive(Debug)]
struct WsWriter {
    inner: ws::WsWriter,
    out: BytesMut,
}

impl Drop for QuicWriter {
    fn drop(&mut self) {
        // Lets the server reap us now instead of at its idle timeout;
        // `Connection::shutdown` is the awaited, guaranteed form.
        self.connection.close(0u32.into(), b"phux: detach");
    }
}

fn default_client_name() -> String {
    format!("phux-client/{}", env!("CARGO_PKG_VERSION"))
}

impl Connection {
    /// An unnegotiated connection over the given transport halves.
    const fn new(
        reader: FrameReader,
        writer: FrameWriter,
        peer_pid: Option<i32>,
        multistream: Option<Multistream>,
    ) -> Self {
        Self {
            reader,
            writer,
            peer_pid,
            negotiated_bootstrap: None,
            server_id: None,
            next_attach_id: 1,
            multistream,
        }
    }

    /// An unnegotiated connection over a connected Unix stream.
    fn over_uds(stream: UnixStream) -> Self {
        // Peer credentials are only readable before the split.
        let peer_pid = stream.peer_cred().ok().and_then(|cred| cred.pid());
        let (read, write) = stream.into_split();
        Self::new(
            FrameReader::Uds(UdsReader {
                inner: read,
                buf: BytesMut::with_capacity(8192),
                bootstrap_limits: BootstrapLimits::default(),
            }),
            FrameWriter::Uds(UdsWriter {
                inner: write,
                out: BytesMut::with_capacity(4096),
                corked: false,
            }),
            peer_pid,
            None,
        )
    }

    /// Open the UDS at `socket` and negotiate the control-plane profile
    /// (L3 included, for metadata requests).
    ///
    /// # Errors
    ///
    /// Connect failures, or handshake errors when the server refuses or does
    /// not acknowledge `HELLO`.
    pub async fn connect(socket: &Path) -> Result<Self, AttachError> {
        Self::connect_with_hello(socket, default_client_name(), control_client_caps()).await
    }

    /// Open the UDS and negotiate with the supplied client profile.
    ///
    /// # Errors
    ///
    /// Returns transport, protocol, or server-refusal errors from negotiation.
    pub async fn connect_with_hello(
        socket: &Path,
        client_name: String,
        client_caps: ClientCapabilities,
    ) -> Result<Self, AttachError> {
        let mut conn = Self::connect_uds_transport(socket).await?;
        conn.negotiate(client_name, client_caps).await?;
        Ok(conn)
    }

    async fn connect_uds_transport(socket: &Path) -> Result<Self, AttachError> {
        phux_config::socket::refuse_dev_on_production(socket).map_err(|refusal| {
            AttachError::Io(io::Error::new(io::ErrorKind::PermissionDenied, refusal))
        })?;
        let stream = UnixStream::connect(socket).await.map_err(AttachError::Io)?;
        Ok(Self::over_uds(stream))
    }

    /// Dial a remote QUIC listener and negotiate the control-plane profile.
    ///
    /// # Errors
    ///
    /// Returns transport, protocol, or server-refusal errors from negotiation.
    pub async fn connect_quic(dial: &QuicDial) -> Result<Self, AttachError> {
        Self::connect_quic_with_hello(dial, default_client_name(), control_client_caps()).await
    }

    /// Dial QUIC and negotiate with the supplied client profile.
    ///
    /// # Errors
    ///
    /// Returns transport, protocol, or server-refusal errors from negotiation.
    pub async fn connect_quic_with_hello(
        dial: &QuicDial,
        client_name: String,
        client_caps: ClientCapabilities,
    ) -> Result<Self, AttachError> {
        let mut conn = Self::connect_quic_transport(dial).await?;
        conn.negotiate(client_name, client_caps.with_quic_streams(true))
            .await?;
        Ok(conn)
    }

    #[cfg(feature = "testkit")]
    /// [`Self::connect_quic`] with a short incomplete-Terminal-frame deadline.
    ///
    /// # Errors
    ///
    /// Returns transport, protocol, or server-refusal errors from negotiation.
    pub async fn connect_quic_with_terminal_frame_deadline_for_test(
        dial: &QuicDial,
        terminal_frame_deadline: std::time::Duration,
    ) -> Result<Self, AttachError> {
        let mut conn = Self::connect_quic_transport(dial).await?;
        let Some(multistream) = conn.multistream.as_mut() else {
            return Err(AttachError::Protocol(
                "QUIC transport did not initialize multistream state".to_owned(),
            ));
        };
        multistream.set_terminal_frame_deadline_for_test(terminal_frame_deadline);
        conn.negotiate(
            default_client_name(),
            control_client_caps().with_quic_streams(true),
        )
        .await?;
        Ok(conn)
    }

    async fn connect_quic_transport(dial: &QuicDial) -> Result<Self, AttachError> {
        let (endpoint, connection, send, recv) = quic::dial(dial).await?;
        Ok(Self::new(
            FrameReader::Quic(QuicReader {
                recv,
                buf: BytesMut::with_capacity(8192),
                scratch: vec![0_u8; QUIC_TRY_READ_BYTES].into_boxed_slice(),
                _endpoint: endpoint.clone(),
                _connection: connection.clone(),
                bootstrap_limits: BootstrapLimits::default(),
            }),
            FrameWriter::Quic(QuicWriter {
                send,
                out: BytesMut::with_capacity(4096),
                endpoint,
                connection: connection.clone(),
            }),
            None,
            Some(Multistream::new(connection)),
        ))
    }

    /// Dial a remote WebSocket listener and negotiate the control-plane profile.
    ///
    /// # Errors
    ///
    /// Returns transport, protocol, or server-refusal errors from negotiation.
    pub async fn connect_ws(dial: &WsDial) -> Result<Self, AttachError> {
        Self::connect_ws_with_hello(dial, default_client_name(), control_client_caps()).await
    }

    /// Dial WebSocket and negotiate with the supplied client profile.
    ///
    /// # Errors
    ///
    /// Returns transport, protocol, or server-refusal errors from negotiation.
    pub async fn connect_ws_with_hello(
        dial: &WsDial,
        client_name: String,
        client_caps: ClientCapabilities,
    ) -> Result<Self, AttachError> {
        let mut conn = Self::connect_ws_transport(dial).await?;
        conn.negotiate(client_name, client_caps).await?;
        Ok(conn)
    }

    async fn connect_ws_transport(dial: &WsDial) -> Result<Self, AttachError> {
        let ws = ws::dial(dial).await?;
        let (tx, rx) = futures_util::StreamExt::split(ws);
        Ok(Self::new(
            FrameReader::Ws(WsReader {
                inner: ws::WsReader::new(rx),
                bootstrap_limits: BootstrapLimits::default(),
            }),
            FrameWriter::Ws(WsWriter {
                inner: ws::WsWriter { tx },
                out: BytesMut::with_capacity(4096),
            }),
            None,
            None,
        ))
    }

    /// The server's pid, from the UDS peer credentials (an OS fact, not a wire
    /// exchange). `None` on remote transports or when the platform omits it.
    #[must_use]
    pub const fn peer_pid(&self) -> Option<i32> {
        self.peer_pid
    }

    /// Close cleanly. On QUIC this sends `CONNECTION_CLOSE` and awaits
    /// `wait_idle`, so the server reaps the consumer at once; elsewhere
    /// dropping the halves already is a clean close.
    pub async fn shutdown(mut self) {
        self.unbind_all_terminals();
        if let FrameWriter::Quic(writer) = &self.writer {
            writer.connection.close(0u32.into(), b"phux: detach");
            writer.endpoint.wait_idle().await;
        }
    }

    /// Connect over `dial` and negotiate the control-plane profile.
    ///
    /// # Errors
    ///
    /// Returns transport, protocol, or server-refusal errors from negotiation.
    pub async fn connect_dial(dial: &Dial) -> Result<Self, AttachError> {
        Self::connect_dial_with_hello(dial, default_client_name(), control_client_caps()).await
    }

    /// Connect over `dial` and negotiate with the supplied client profile.
    ///
    /// # Errors
    ///
    /// Returns transport, protocol, or server-refusal errors from negotiation.
    pub async fn connect_dial_with_hello(
        dial: &Dial,
        client_name: String,
        client_caps: ClientCapabilities,
    ) -> Result<Self, AttachError> {
        match dial {
            Dial::Uds(path) => Self::connect_with_hello(path, client_name, client_caps).await,
            Dial::Quic(quic) => Self::connect_quic_with_hello(quic, client_name, client_caps).await,
            Dial::Ws(ws) => Self::connect_ws_with_hello(ws, client_name, client_caps).await,
        }
    }

    /// An unnegotiated connection over an already-connected [`UnixStream`]:
    /// the in-process test seam.
    #[cfg(any(test, feature = "testkit"))]
    #[must_use]
    pub fn from_stream(stream: UnixStream) -> Self {
        Self::over_uds(stream)
    }

    /// Negotiate the protocol once. Production constructors already did; this
    /// is public for tests driving a `from_stream` connection.
    ///
    /// # Errors
    ///
    /// Any handshake failure, as [`AttachError`].
    pub async fn negotiate(
        &mut self,
        client_name: String,
        client_caps: ClientCapabilities,
    ) -> Result<(), AttachError> {
        if self.negotiated_bootstrap.is_some() {
            return Err(AttachError::Protocol(
                "HELLO negotiation already completed on this connection".to_owned(),
            ));
        }
        self.writer
            .send(&FrameKind::Hello {
                client_name,
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                client_caps,
            })
            .await?;
        match self.recv().await? {
            FrameKind::HelloOk {
                protocol_major,
                protocol_minor,
                protocol_patch,
                server_caps,
                server_id,
                selected_profile,
                bootstrap_limits,
            } => {
                validate_hello_ok(
                    &client_caps,
                    protocol_major,
                    protocol_minor,
                    protocol_patch,
                    selected_profile,
                    bootstrap_limits,
                )
                .map_err(|err| AttachError::Protocol(err.to_string()))?;
                self.reader.set_bootstrap_limits(bootstrap_limits);
                self.negotiated_bootstrap = Some(NegotiatedBootstrap {
                    profile: selected_profile,
                    limits: bootstrap_limits,
                    server_features: server_caps.features,
                });
                self.server_id = Some(server_id);
                Ok(())
            }
            FrameKind::Error { message, .. } => Err(AttachError::Refused(message)),
            _ => Err(AttachError::Protocol(crate::explain::unexpected_reply(
                "HELLO",
            ))),
        }
    }

    /// What `HELLO_OK` selected; `None` only on the `from_stream` test seam.
    #[must_use]
    pub const fn negotiated_bootstrap(&self) -> Option<NegotiatedBootstrap> {
        self.negotiated_bootstrap
    }

    /// The role an observer attaches with (ADR-0127): `VIEWER` when the server
    /// advertises `ATTACH_ROLES`, so it can never type into what it watches.
    #[must_use]
    pub fn observer_role_policy(&self) -> Option<phux_protocol::wire::frame::RolePolicy> {
        self.advertises(ServerFeature::AttachRoles)
            .then_some(phux_protocol::wire::frame::RolePolicy::VIEWER)
    }

    /// Whether `HELLO_OK` advertised `feature`.
    fn advertises(&self, feature: ServerFeature) -> bool {
        self.negotiated_bootstrap
            .is_some_and(|negotiated| negotiated.server_features.contains(feature))
    }

    /// `HELLO_OK.server_id`, the server-incarnation id (ADR-0053).
    #[must_use]
    pub fn server_id(&self) -> Option<&[u8]> {
        self.server_id.as_deref()
    }

    /// Allocate a connection-local, non-zero correlation id for an `ATTACH`.
    pub const fn next_attach_id(&mut self) -> u32 {
        let id = self.next_attach_id;
        self.next_attach_id = self.next_attach_id.wrapping_add(1);
        if self.next_attach_id == 0 {
            self.next_attach_id = 1;
        }
        id
    }
    /// Encode `frame` and write it to the server.
    pub async fn send(&mut self, frame: &FrameKind) -> Result<(), AttachError> {
        if let Some(terminal_id) = terminal_target(frame)
            && self.multistream_enabled()
        {
            let Some(mux) = self.multistream.as_mut() else {
                return Err(AttachError::Protocol(
                    "negotiated QUIC stream state is unavailable".to_owned(),
                ));
            };
            let Some(binding) = mux.bindings.get_mut(terminal_id) else {
                if mux.ended.contains(terminal_id) {
                    tracing::debug!(
                        ?terminal_id,
                        "Terminal frame dropped: its stream already ended",
                    );
                    return Ok(());
                }
                return Err(AttachError::Protocol(format!(
                    "Terminal frame requires a live QUIC binding: {terminal_id:?}"
                )));
            };
            return send_quic_frame(&mut binding.send, frame).await;
        }
        self.writer.send(frame).await
    }

    /// Whether a Terminal-targeted frame for `terminal_id` can be sent now.
    ///
    /// Always `true` off QUIC multistream, where every frame rides the one
    /// control stream. On multistream it is `true` once the Terminal's stream
    /// is bound (or has already ended, whose frames [`Self::send`] drops):
    /// a Terminal named only by a persisted layout, whose attach has not
    /// been confirmed, has no stream yet, and sending to it is the caller
    /// bug [`Self::send`] refuses.
    #[must_use]
    pub fn can_route_terminal(&self, terminal_id: &ResourceId) -> bool {
        if !self.multistream_enabled() {
            return true;
        }
        self.multistream.as_ref().is_some_and(|mux| {
            mux.bindings.contains_key(terminal_id) || mux.ended.contains(terminal_id)
        })
    }

    /// Whether this negotiated QUIC connection supports per-Terminal
    /// streams. The feature is intentionally gated by both the negotiated
    /// bit and the transport variant: UDS/WS peers must retain their one
    /// stream shape even if a future server advertises unrelated bits.
    #[must_use]
    pub fn multistream_enabled(&self) -> bool {
        matches!(self.writer, FrameWriter::Quic(_)) && self.advertises(ServerFeature::QuicStreams)
    }

    /// Open and bind a QUIC stream for `terminal_id` (a no-op when multistream
    /// is off or it is already bound). The bind header carries an app-level
    /// stream id, never the QUIC one.
    pub async fn bind_terminal(&mut self, terminal_id: &ResourceId) -> Result<(), AttachError> {
        if !self.multistream_enabled() {
            return Ok(());
        }
        if self
            .multistream
            .as_ref()
            .is_some_and(|mux| mux.bindings.contains_key(terminal_id))
        {
            return Ok(());
        }
        if self
            .multistream
            .as_ref()
            .is_some_and(|mux| mux.bindings.len() >= MAX_CLIENT_TERMINAL_STREAMS)
        {
            return Err(AttachError::Protocol(format!(
                "QUIC Terminal stream cap exceeded ({MAX_CLIENT_TERMINAL_STREAMS})"
            )));
        }

        if self.multistream.is_none() {
            let connection = match &self.writer {
                FrameWriter::Quic(writer) => writer.connection.clone(),
                FrameWriter::Uds(_) | FrameWriter::Ws(_) => return Ok(()),
            };
            self.multistream = Some(Multistream::new(connection));
        }

        let Some(mux) = self.multistream.as_mut() else {
            return Err(AttachError::Protocol(
                "QUIC multi-stream state was not initialized".to_owned(),
            ));
        };
        let Some(stream_id) = mux.next_stream_id() else {
            return Err(AttachError::Protocol(
                "QUIC Terminal stream id exhausted".to_owned(),
            ));
        };
        let (mut send, recv) =
            tokio::time::timeout(TERMINAL_STREAM_OPEN_DEADLINE, mux.conn.open_bi())
                .await
                .map_err(|_| AttachError::Connect("opening Terminal stream timed out".to_owned()))?
                .map_err(|err| AttachError::Connect(format!("opening Terminal stream: {err}")))?;
        let mut header = BytesMut::with_capacity(64);
        phux_protocol::wire::stream_bind::encode(
            &phux_protocol::wire::stream_bind::StreamBind {
                terminal_id: terminal_id.clone(),
                stream_id,
            },
            &mut header,
        );
        tokio::time::timeout(TERMINAL_STREAM_OPEN_DEADLINE, send.write_all(&header))
            .await
            .map_err(|_| AttachError::Connect("writing STREAM_BIND timed out".to_owned()))?
            .map_err(|err| AttachError::Io(io::Error::other(err)))?;

        let frame_bytes = mux.frame_bytes.clone();
        let ended_terminal = terminal_id.clone();
        let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let receive_task = tokio::spawn(pump_terminal_stream(
            recv,
            ended_terminal,
            stream_id,
            mux.frames_tx.clone(),
            frame_bytes,
            std::sync::Arc::clone(&active),
            mux.terminal_frame_deadline(),
        ));
        if let Some(mut replaced) = mux.bindings.insert(
            terminal_id.clone(),
            MuxBinding {
                stream_id,
                send,
                receive_task: receive_task.abort_handle(),
                active,
            },
        ) {
            replaced.retire();
        }
        mux.ended.remove(terminal_id);
        Ok(())
    }

    /// Finish and forget one Terminal stream, if it is bound.
    pub fn unbind_terminal(&mut self, terminal_id: &ResourceId) {
        if let Some(mux) = self.multistream.as_mut()
            && let Some(mut binding) = mux.bindings.remove(terminal_id)
        {
            binding.retire();
        }
    }

    /// Finish all client-opened Terminal streams.
    pub fn unbind_all_terminals(&mut self) {
        if let Some(mux) = self.multistream.as_mut() {
            for (_, mut binding) in mux.bindings.drain() {
                binding.retire();
            }
            mux.ended.clear();
        }
    }

    /// Accumulate sends until [`Self::uncork`], so one input batch costs one
    /// write. Callers MUST uncork on every exit path, errors included.
    pub fn cork(&mut self) {
        self.writer.cork();
    }

    /// Ship everything corked since [`Self::cork`], in send order, in one
    /// write. A no-op if nothing was sent or the transport does not cork.
    pub async fn uncork(&mut self) -> Result<(), AttachError> {
        self.writer.uncork().await
    }

    /// Read the next frame from the server.
    ///
    /// On WebSocket the read also ping/pong probes an idle peer, so one that
    /// stops answering surfaces as [`AttachError::Disconnected`]. A SPEC §5
    /// framing violation is answered with `ERROR { FRAME_TOO_LARGE }`
    /// (best-effort) before [`AttachError::Framing`] is returned.
    pub async fn recv(&mut self) -> Result<FrameKind, AttachError> {
        let result = self.recv_frame().await;
        if let Err(err) = &result {
            self.emit_frame_too_large(err).await;
        }
        result
    }

    async fn recv_frame(&mut self) -> Result<FrameKind, AttachError> {
        if self.multistream.is_some() {
            return self.recv_multistream().await;
        }
        match (&mut self.reader, &mut self.writer) {
            (FrameReader::Ws(reader), FrameWriter::Ws(writer)) => {
                reader.recv_alive(&mut writer.inner).await
            }
            (reader, _) => reader.recv().await,
        }
    }

    /// SPEC §5: send `ERROR { FRAME_TOO_LARGE }` before the caller drops
    /// the connection. Best-effort: a vanished peer cannot fail the close.
    async fn emit_frame_too_large(&mut self, err: &AttachError) {
        let AttachError::Framing(violation) = err else {
            return;
        };
        // Drop any corked batch so the ERROR is not stuck behind it.
        if let FrameWriter::Uds(writer) = &mut self.writer {
            writer.corked = false;
            writer.out.clear();
        }
        let _ = self
            .writer
            .send(&framing::frame_too_large_error(*violation))
            .await;
    }

    async fn recv_multistream(&mut self) -> Result<FrameKind, AttachError> {
        let limits = self
            .negotiated_bootstrap
            .map_or_else(BootstrapLimits::default, |negotiated| negotiated.limits);
        loop {
            let Some(mux) = self.multistream.as_mut() else {
                return Err(AttachError::Protocol(
                    "QUIC multi-stream state disappeared".to_owned(),
                ));
            };
            let reader = &mut self.reader;
            tokio::select! {
                result = reader.recv() => return result,
                Some(item) = mux.frames_rx.recv() => {
                    match item {
                        MuxItem::Frame(received) => {
                            if !mux_frame_is_current(mux, &received) {
                                continue;
                            }
                            let mut bytes = received.bytes.map_err(AttachError::Protocol)?;
                            return decode_buffered(&mut bytes, limits)?.ok_or_else(||
                                AttachError::Protocol("Terminal stream ended with a partial frame".to_owned())
                            );
                        }
                        MuxItem::End { terminal_id, stream_id, active } => {
                            mux.end_stream(terminal_id, stream_id, &active);
                        }
                    }
                }
            }
        }
    }

    /// A frame that is already available without waiting, or `Ok(None)`, so
    /// the attach loop can drain a burst into one paint. Framing violations
    /// are answered as in [`Self::recv`], without blocking.
    pub fn try_recv(&mut self) -> Result<Option<FrameKind>, AttachError> {
        let result = self.try_recv_frame();
        if let Err(err) = &result {
            let _ = futures_util::FutureExt::now_or_never(self.emit_frame_too_large(err));
        }
        result
    }

    fn try_recv_frame(&mut self) -> Result<Option<FrameKind>, AttachError> {
        let limits = self
            .negotiated_bootstrap
            .map_or_else(BootstrapLimits::default, |negotiated| negotiated.limits);
        if let Some(mux) = self.multistream.as_mut() {
            while let Ok(item) = mux.frames_rx.try_recv() {
                match item {
                    MuxItem::Frame(frame) => {
                        if !mux_frame_is_current(mux, &frame) {
                            continue;
                        }
                        let mut bytes = frame.bytes.map_err(AttachError::Protocol)?;
                        return decode_buffered(&mut bytes, limits);
                    }
                    MuxItem::End {
                        terminal_id,
                        stream_id,
                        active,
                    } => mux.end_stream(terminal_id, stream_id, &active),
                }
            }
        }
        self.reader.try_recv()
    }

    /// Send one `COMMAND` and wait for its reply, keeping every frame the
    /// server interleaved ahead of it (see the interleave contract on
    /// [`Connection`]).
    ///
    /// The frames come back inside [`Reply`], so the ack is unreachable
    /// without deciding what to do with them; the only way to drop them is
    /// the greppable [`Reply::into_result_ignoring_interleaved`].
    ///
    /// The reply is a `COMMAND_RESULT` for `request_id`, or an `ERROR`
    /// correlated to it (proto.md §9), folded into [`CommandResult::Error`];
    /// without that a peer refusing an unimplemented command would hang the
    /// caller. An uncorrelated `ERROR` (a hub's satellite degradation notice)
    /// is not an answer and lands in [`Reply::interleaved`].
    ///
    /// # Errors
    ///
    /// Transport and decode failures; a server that closes without replying
    /// is [`AttachError::Disconnected`].
    pub async fn request(
        &mut self,
        request_id: u32,
        command: Command,
    ) -> Result<Reply, AttachError> {
        let frame = FrameKind::Command {
            request_id,
            command,
        };
        let (answer, interleaved) = self
            .round_trip(request_id, &frame, |frame| match frame {
                FrameKind::CommandResult { request_id, result } => {
                    Some((*request_id, result.clone()))
                }
                _ => None,
            })
            .await?
            .into_parts();
        // `CommandResult::Error` carries exactly the `ERROR` payload.
        let result = answer.unwrap_or_else(|refusal| CommandResult::Error {
            code: refusal.code,
            message: refusal.message,
        });
        Ok(Reply {
            result,
            interleaved,
        })
    }

    /// Send one `GET_METADATA` and wait for its `METADATA_VALUE`, keeping
    /// interleaved frames. A refusal is an `Err` answer, distinct from an
    /// unset key (`Ok(None)`).
    ///
    /// # Errors
    ///
    /// Propagates transport and decode failures from [`Self::send`] /
    /// [`Self::recv`].
    pub async fn request_metadata(
        &mut self,
        request_id: u32,
        scope: Scope,
        key: String,
    ) -> Result<Reply<Answer<Option<Vec<u8>>>>, AttachError> {
        let frame = FrameKind::GetMetadata {
            request_id,
            scope,
            key,
        };
        self.round_trip(request_id, &frame, |frame| match frame {
            FrameKind::MetadataValue { request_id, value } => Some((*request_id, value.clone())),
            _ => None,
        })
        .await
    }

    /// Send one `LIST_METADATA` and wait for its `METADATA_KEYS`, keeping
    /// interleaved frames.
    ///
    /// # Errors
    ///
    /// Propagates transport and decode failures from [`Self::send`] /
    /// [`Self::recv`].
    pub async fn request_metadata_keys(
        &mut self,
        request_id: u32,
        scope: Scope,
    ) -> Result<Reply<Answer<Vec<String>>>, AttachError> {
        let frame = FrameKind::ListMetadata { request_id, scope };
        self.round_trip(request_id, &frame, |frame| match frame {
            FrameKind::MetadataKeys { request_id, keys } => Some((*request_id, keys.clone())),
            _ => None,
        })
        .await
    }

    /// Send one `SPAWN_RESOURCE` and wait for its `RESOURCE_SPAWNED`, keeping
    /// interleaved frames. The correlation id is read from `frame`. A
    /// satellite may answer a relayed spawn with a correlated `ERROR`, which
    /// becomes an `Err` answer.
    ///
    /// # Errors
    ///
    /// [`AttachError::Protocol`] when `frame` is not a `SPAWN_RESOURCE`;
    /// otherwise transport and decode failures.
    pub async fn request_spawn(
        &mut self,
        frame: &FrameKind,
    ) -> Result<Reply<Answer<SpawnResult>>, AttachError> {
        let FrameKind::SpawnResource { request_id, .. } = frame else {
            return Err(AttachError::Protocol(format!(
                "request_spawn needs a SPAWN_RESOURCE frame, got {frame:?}",
            )));
        };
        self.round_trip(*request_id, frame, |frame| match frame {
            FrameKind::ResourceSpawned { request_id, result } => {
                Some((*request_id, result.clone()))
            }
            _ => None,
        })
        .await
    }

    /// Send one `MOVE_RESOURCE` and wait for its `RESOURCE_MOVED`, keeping
    /// interleaved frames (ADR-0056). The correlation id is read from `frame`.
    ///
    /// # Errors
    ///
    /// [`AttachError::Protocol`] when `frame` is not a `MOVE_RESOURCE`;
    /// otherwise transport and decode failures.
    pub async fn request_move(
        &mut self,
        frame: &FrameKind,
    ) -> Result<Reply<Answer<MoveResult>>, AttachError> {
        let FrameKind::MoveResource { request_id, .. } = frame else {
            return Err(AttachError::Protocol(format!(
                "request_move needs a MOVE_RESOURCE frame, got {frame:?}",
            )));
        };
        self.round_trip(*request_id, frame, |frame| match frame {
            FrameKind::ResourceMoved { request_id, result } => Some((*request_id, result.clone())),
            _ => None,
        })
        .await
    }

    /// Send `frame`, then wait for its answer; see [`Self::await_answer`].
    async fn round_trip<T>(
        &mut self,
        request_id: u32,
        frame: &FrameKind,
        recognize: impl Fn(&FrameKind) -> Option<(u32, T)>,
    ) -> Result<Reply<Answer<T>>, AttachError> {
        self.send(frame).await?;
        let mut interleaved = Vec::new();
        let result = self
            .await_answer(request_id, &mut interleaved, recognize)
            .await?;
        Ok(Reply {
            result,
            interleaved,
        })
    }

    /// The workspace's only correlation loop: read until the peer answers
    /// `request_id`, pushing every other frame onto `interleaved`.
    ///
    /// `recognize` names one pair's reply frame; the correlated-`ERROR` arm
    /// lives here so no pair can forget it. It borrows each frame and clones
    /// the payload out, so an unrecognized frame is never consumed.
    ///
    /// # Errors
    ///
    /// Transport and decode failures; a peer that closes without answering is
    /// [`AttachError::Disconnected`].
    async fn await_answer<T>(
        &mut self,
        request_id: u32,
        interleaved: &mut Vec<FrameKind>,
        recognize: impl Fn(&FrameKind) -> Option<(u32, T)>,
    ) -> Result<Answer<T>, AttachError> {
        loop {
            let frame = self.recv().await?;
            if let Some((got, value)) = recognize(&frame)
                && got == request_id
            {
                return Ok(Ok(value));
            }
            // proto.md §9: an `ERROR` carrying this `request_id` answers it;
            // an uncorrelated one is a pushed notice.
            if let FrameKind::Error {
                request_id: Some(got),
                code,
                message,
            } = &frame
                && *got == request_id
            {
                return Ok(Err(Refusal {
                    code: *code,
                    message: message.clone(),
                }));
            }
            interleaved.push(frame);
        }
    }
}

/// The peer's correlated `ERROR` answer to one request (proto.md §9). Its
/// `Display` renders the code as words, since it reaches users verbatim.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}: {message}", crate::explain::error_code_label(*.code))]
pub struct Refusal {
    /// The typed code the peer refused with.
    pub code: ErrorCode,
    /// The peer's human-readable explanation.
    pub message: String,
}

/// A peer's answer to one correlated request: the reply payload, or the
/// [`Refusal`] it answered with instead.
pub type Answer<T> = Result<T, Refusal>;

/// One correlated round trip's answer plus every frame the peer pushed ahead
/// of it. Opaque on purpose: the fields are reachable only through methods,
/// so neither half can be destructured away by omission.
#[derive(Debug)]
#[must_use = "the reply carries frames the server will never re-send; dropping \
              it loses them"]
pub struct Reply<T = CommandResult> {
    /// The pair's answer: its reply-frame payload, or the peer's refusal.
    result: T,
    /// Frames observed while waiting, in arrival order.
    interleaved: Vec<FrameKind>,
}

impl<T> Reply<T> {
    /// The answer and the interleaved frames, in arrival order.
    #[must_use]
    pub fn into_parts(self) -> (T, Vec<FrameKind>) {
        (self.result, self.interleaved)
    }

    /// Borrow the answer without consuming the reply.
    #[must_use]
    pub const fn result(&self) -> &T {
        &self.result
    }

    /// Borrow the frames the server interleaved ahead of the ack.
    #[must_use]
    pub fn interleaved(&self) -> &[FrameKind] {
        &self.interleaved
    }

    /// Take the answer and drop the interleaved frames.
    ///
    /// Only correct when the server provably pushes nothing before the ack
    /// (no frame from the handler, no subscription on the connection); cite
    /// the handler at each call site. A non-empty drop is logged at `warn`.
    #[must_use]
    pub fn into_result_ignoring_interleaved(self) -> T {
        if !self.interleaved.is_empty() {
            tracing::warn!(
                dropped = self.interleaved.len(),
                frames = ?self.interleaved,
                "correlated reply discarded frames the server interleaved ahead \
                 of the answer; the server will not re-send them",
            );
        }
        self.result
    }
}

impl FrameWriter {
    async fn send(&mut self, frame: &FrameKind) -> Result<(), AttachError> {
        match self {
            Self::Uds(w) => w.send(frame).await,
            Self::Quic(w) => w.send(frame).await,
            Self::Ws(w) => w.send(frame).await,
        }
    }

    /// Batch sends until [`Self::uncork`]. UDS only: QUIC coalesces in its own
    /// send buffer, and WebSocket needs one message per frame.
    fn cork(&mut self) {
        if let Self::Uds(w) = self {
            w.cork();
        }
    }

    /// Ship a corked batch. See [`Self::cork`].
    async fn uncork(&mut self) -> Result<(), AttachError> {
        match self {
            Self::Uds(w) => w.uncork().await,
            Self::Quic(_) | Self::Ws(_) => Ok(()),
        }
    }
}

impl FrameReader {
    async fn recv(&mut self) -> Result<FrameKind, AttachError> {
        match self {
            Self::Uds(r) => r.recv().await,
            Self::Quic(r) => r.recv().await,
            Self::Ws(r) => r.recv().await,
        }
    }

    const fn set_bootstrap_limits(&mut self, limits: BootstrapLimits) {
        match self {
            Self::Uds(reader) => reader.bootstrap_limits = limits,
            Self::Quic(reader) => reader.bootstrap_limits = limits,
            Self::Ws(reader) => reader.bootstrap_limits = limits,
        }
    }

    /// Non-blocking [`Self::recv`]: `Ok(None)` when no complete frame is
    /// available without waiting.
    fn try_recv(&mut self) -> Result<Option<FrameKind>, AttachError> {
        match self {
            Self::Uds(r) => r.try_recv(),
            Self::Quic(r) => r.try_recv(),
            Self::Ws(r) => r.try_recv(),
        }
    }
}

/// Name the frame a failed write was carrying. The [`io::ErrorKind`] is kept:
/// the TUI classifies a gone peer on it.
fn write_failed(frame: &FrameKind, err: &io::Error) -> AttachError {
    AttachError::Io(io::Error::new(
        err.kind(),
        format!("sending frame type 0x{:02x}: {err}", frame.type_byte()),
    ))
}

/// [`write_failed`] for a corked batch, which has no single frame to name.
fn batch_write_failed(err: &io::Error) -> AttachError {
    AttachError::Io(io::Error::new(
        err.kind(),
        format!("sending a batched input write: {err}"),
    ))
}

impl UdsWriter {
    /// Write one frame, or append it to the batch while corked; wire order is
    /// the same either way.
    async fn send(&mut self, frame: &FrameKind) -> Result<(), AttachError> {
        if self.corked {
            frame.encode(&mut self.out);
            return Ok(());
        }
        self.out.clear();
        frame.encode(&mut self.out);
        self.inner
            .write_all(&self.out)
            .await
            .map_err(|err| write_failed(frame, &err))?;
        self.inner
            .flush()
            .await
            .map_err(|err| write_failed(frame, &err))?;
        Ok(())
    }

    /// Start a batch. Clears the buffer, so a skipped uncork can never leak
    /// stale bytes into the next batch.
    fn cork(&mut self) {
        self.out.clear();
        self.corked = true;
    }

    /// Ship everything accumulated since [`Self::cork`] in one write.
    async fn uncork(&mut self) -> Result<(), AttachError> {
        self.corked = false;
        if self.out.is_empty() {
            return Ok(());
        }
        let batched = self.inner.write_all(&self.out).await;
        self.out.clear();
        batched.map_err(|err| batch_write_failed(&err))?;
        self.inner
            .flush()
            .await
            .map_err(|err| batch_write_failed(&err))?;
        Ok(())
    }
}

/// Read chunks from `src` into `buf` until one complete frame decodes; EOF is
/// [`AttachError::Disconnected`].
async fn recv_buffered(
    src: &mut (impl tokio::io::AsyncRead + Unpin),
    buf: &mut BytesMut,
    limits: BootstrapLimits,
) -> Result<FrameKind, AttachError> {
    loop {
        if let Some(frame) = decode_buffered(buf, limits)? {
            return Ok(frame);
        }
        if src.read_buf(buf).await.map_err(AttachError::Io)? == 0 {
            return Err(AttachError::Disconnected);
        }
    }
}

impl UdsReader {
    async fn recv(&mut self) -> Result<FrameKind, AttachError> {
        recv_buffered(&mut self.inner, &mut self.buf, self.bootstrap_limits).await
    }

    fn try_recv(&mut self) -> Result<Option<FrameKind>, AttachError> {
        if let Some(frame) = decode_buffered(&mut self.buf, self.bootstrap_limits)? {
            return Ok(Some(frame));
        }
        match self.inner.try_read_buf(&mut self.buf) {
            Ok(0) => return Err(AttachError::Disconnected),
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(err) => return Err(AttachError::Io(err)),
        }
        decode_buffered(&mut self.buf, self.bootstrap_limits)
    }
}

impl QuicWriter {
    async fn send(&mut self, frame: &FrameKind) -> Result<(), AttachError> {
        self.out.clear();
        frame.encode(&mut self.out);
        self.send
            .write_all(&self.out)
            .await
            .map_err(|err| AttachError::Io(io::Error::other(err)))?;
        Ok(())
    }
}

impl QuicReader {
    async fn recv(&mut self) -> Result<FrameKind, AttachError> {
        recv_buffered(&mut self.recv, &mut self.buf, self.bootstrap_limits).await
    }

    /// Drain a buffered frame, topping up from quinn if it already holds
    /// bytes. quinn has no `try_read`, so [`Self::poll_read_once`] polls the
    /// read once against a no-op waker; that is sound because quinn reads are
    /// cancel-safe and the next `recv().await` re-polls with a real waker.
    fn try_recv(&mut self) -> Result<Option<FrameKind>, AttachError> {
        if let Some(frame) = decode_buffered(&mut self.buf, self.bootstrap_limits)? {
            return Ok(Some(frame));
        }
        match self.poll_read_once()? {
            // A clean finish: the next `recv` reports `Disconnected`.
            Some(0) | None => Ok(None),
            Some(_) => decode_buffered(&mut self.buf, self.bootstrap_limits),
        }
    }

    /// Poll one read into `self.buf` against a no-op waker: `Ok(None)` when
    /// pending, else the byte count. Reads land in the reused `scratch`
    /// (this crate forbids `unsafe`, and zero-filling `buf` per call would
    /// memset the window on every drain turn).
    fn poll_read_once(&mut self) -> Result<Option<usize>, AttachError> {
        use tokio::io::AsyncRead;
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        let mut read_buf = tokio::io::ReadBuf::new(&mut self.scratch);
        let polled = std::pin::Pin::new(&mut self.recv).poll_read(&mut cx, &mut read_buf);
        match polled {
            std::task::Poll::Pending => Ok(None),
            std::task::Poll::Ready(Err(err)) => Err(AttachError::Io(err)),
            std::task::Poll::Ready(Ok(())) => {
                let filled = read_buf.filled().len();
                self.buf.extend_from_slice(&self.scratch[..filled]);
                Ok(Some(filled))
            }
        }
    }
}

impl WsWriter {
    async fn send(&mut self, frame: &FrameKind) -> Result<(), AttachError> {
        self.out.clear();
        frame.encode(&mut self.out);
        self.inner.send(&self.out).await.map_err(AttachError::from)
    }
}

impl WsReader {
    async fn recv(&mut self) -> Result<FrameKind, AttachError> {
        let message = self.inner.recv_message().await?;
        self.decode(message)
    }

    /// [`Self::recv`] with RFC 6455 liveness, which needs the write half.
    async fn recv_alive(&mut self, writer: &mut ws::WsWriter) -> Result<FrameKind, AttachError> {
        let message = ws::recv_message_alive(&mut self.inner, writer).await?;
        self.decode(message)
    }

    /// A message tungstenite already decoded, if any. A close seen here is
    /// also `Ok(None)`; the next `recv` reports it.
    fn try_recv(&mut self) -> Result<Option<FrameKind>, AttachError> {
        let Some(message) = self.inner.try_recv_message()? else {
            return Ok(None);
        };
        self.decode(Some(message)).map(Some)
    }

    fn decode(&self, message: Option<Vec<u8>>) -> Result<FrameKind, AttachError> {
        let Some(frame) = message else {
            return Err(AttachError::Disconnected);
        };
        // One message is exactly one frame; `check_frame` rejects a length
        // that disagrees with the message, so the decode leaves no tail.
        framing::check_frame(&frame)?;
        let (decoded, _rest) = FrameKind::decode_with_limits(&frame, self.bootstrap_limits)
            .map_err(|err| {
                AttachError::Protocol(format!("server sent undecodable frame: {err:?}"))
            })?;
        Ok(decoded)
    }
}

/// Decode and consume one complete frame from the front of `buf`, or
/// `Ok(None)` while it is still partial.
fn decode_buffered(
    buf: &mut BytesMut,
    bootstrap_limits: BootstrapLimits,
) -> Result<Option<FrameKind>, AttachError> {
    let Some(framed) = framing::split_frame(buf)? else {
        return Ok(None);
    };
    let (frame, _rest) = FrameKind::decode_with_limits(&framed, bootstrap_limits)
        .map_err(|err| AttachError::Protocol(format!("server sent undecodable frame: {err:?}")))?;
    Ok(Some(frame))
}

const fn control_client_caps() -> ClientCapabilities {
    ClientCapabilities::new().with_layers(LayerSet::with(&[Layer::L3]))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn framed(seq: u64) -> BytesMut {
        let mut buf = BytesMut::new();
        seq_ack(seq).encode(&mut buf);
        buf
    }

    /// A frame carrying `seq`, for ordering assertions.
    fn seq_ack(seq: u64) -> FrameKind {
        FrameKind::FrameAck {
            terminal_id: phux_protocol::ids::ResourceId::Local { id: 1 },
            stream_id: phux_protocol::StreamId::new(1).expect("stream"),
            bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
            seq,
        }
    }

    /// Build a `UdsWriter` over one end of a socket pair, handing back the
    /// other end to read what it wrote.
    fn writer_pair() -> (UdsWriter, UnixStream) {
        let (near, far) = UnixStream::pair().expect("socketpair");
        let (_read, write) = near.into_split();
        (
            UdsWriter {
                inner: write,
                out: BytesMut::with_capacity(64),
                corked: false,
            },
            far,
        )
    }

    /// Nothing reaches the peer while corked (`write_all` would already have
    /// returned), and everything arrives in send order on the uncork.
    #[tokio::test]
    async fn a_corked_batch_is_withheld_then_shipped_in_order() {
        let (mut writer, peer) = writer_pair();
        writer.cork();
        for seq in 1..=3 {
            writer.send(&seq_ack(seq)).await.expect("corked send");
        }
        let mut early = [0u8; 64];
        assert!(
            matches!(peer.try_read(&mut early), Err(ref err) if err.kind() == io::ErrorKind::WouldBlock),
            "corked frames must not reach the peer before the uncork",
        );

        writer.uncork().await.expect("uncork");
        let mut buf = BytesMut::new();
        let mut chunk = [0u8; 256];
        while buf.len() < 3 * framed(1).len() {
            peer.readable().await.expect("readable");
            match peer.try_read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {}
                Err(err) => panic!("read: {err}"),
            }
        }
        for seq in 1..=3 {
            let decoded = decode_buffered(&mut buf, BootstrapLimits::default())
                .expect("decode")
                .expect("frame present");
            assert_eq!(decoded, seq_ack(seq), "corked frames must keep send order");
        }
        assert!(buf.is_empty(), "the batch must be exactly what was corked");
    }

    /// The uncork must un-cork, or every later frame is silently swallowed.
    #[tokio::test]
    async fn a_send_after_uncork_writes_straight_through() {
        let (mut writer, peer) = writer_pair();
        writer.cork();
        writer.uncork().await.expect("uncork with nothing corked");

        writer.send(&seq_ack(9)).await.expect("send");
        let mut buf = BytesMut::new();
        let mut chunk = [0u8; 256];
        peer.readable().await.expect("readable");
        let n = peer.try_read(&mut chunk).expect("read");
        buf.extend_from_slice(&chunk[..n]);
        let decoded = decode_buffered(&mut buf, BootstrapLimits::default())
            .expect("decode")
            .expect("frame present");
        assert_eq!(decoded, seq_ack(9));
    }

    /// One read can hold several frames and a partial tail: peel them in
    /// order and keep the tail until the rest arrives.
    #[test]
    fn decode_buffered_drains_whole_frames_and_holds_a_partial_one() {
        let mut buf = BytesMut::new();
        for seq in 1..=3 {
            buf.extend_from_slice(&framed(seq));
        }
        let mut seqs = Vec::new();
        while let Some(FrameKind::FrameAck { seq, .. }) =
            decode_buffered(&mut buf, BootstrapLimits::default()).expect("decode")
        {
            seqs.push(seq);
        }
        assert_eq!(seqs, vec![1, 2, 3]);
        assert!(buf.is_empty(), "fully consumed buffer");

        let whole = framed(7);
        let cut = whole.len() - 2;
        buf.extend_from_slice(&whole[..cut]);
        assert!(
            decode_buffered(&mut buf, BootstrapLimits::default())
                .expect("partial")
                .is_none(),
            "incomplete frame yields None"
        );
        assert_eq!(buf.len(), cut, "partial bytes retained");
        // Deliver the tail; now it decodes and the buffer drains.
        buf.extend_from_slice(&whole[cut..]);
        let frame = decode_buffered(&mut buf, BootstrapLimits::default()).expect("complete");
        assert!(matches!(frame, Some(FrameKind::FrameAck { seq: 7, .. })));
        assert!(buf.is_empty());
    }

    // --- correlated requests (scripted server on a LocalSet) -----------

    use bytes::Bytes;
    use phux_protocol::caps::BootstrapStreamProfile;
    use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
    use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, MAX_FRAME_LEN};
    use phux_protocol::wire::framing::FramingError;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        tokio::task::LocalSet::new().block_on(&rt, fut)
    }

    /// A hanging wait is the defect under test, so every round trip is capped.
    const WEDGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

    /// Spawn a server half that plays `script` once the request arrives.
    fn scripted(script: Vec<FrameKind>) -> Connection {
        let (client_stream, server_stream) = UnixStream::pair().expect("pair");
        let mut server = Connection::from_stream(server_stream);
        tokio::task::spawn_local(async move {
            server.recv().await.expect("request frame");
            for frame in &script {
                server.send(frame).await.expect("scripted frame");
            }
        });
        Connection::from_stream(client_stream)
    }

    /// One `COMMAND` round trip against `script`.
    fn request_against(script: Vec<FrameKind>) -> Reply {
        block_on(async {
            let mut client = scripted(script);
            let command = Command::GetState {
                scope: phux_protocol::wire::frame::StateScope::Server,
            };
            tokio::time::timeout(WEDGE_TIMEOUT, client.request(7, command))
                .await
                .expect("the request must resolve")
                .expect("reply")
        })
    }

    fn ack(request_id: u32) -> FrameKind {
        FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        }
    }

    fn bootstrap() -> Vec<FrameKind> {
        let terminal_id = ResourceId::local(1);
        let stream_id = StreamId::new(1).expect("stream");
        let bootstrap_id = BootstrapId::new(1).expect("bootstrap");
        vec![
            FrameKind::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 120,
                rows: 40,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: Bytes::from_static(b"opening screen"),
            },
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
        ]
    }

    #[test]
    fn pre_ack_bootstrap_is_returned_instead_of_discarded() {
        let mut script = bootstrap();
        script.push(ack(7));
        let reply = request_against(script);
        assert!(matches!(reply.result(), CommandResult::Ok));
        assert!(matches!(
            reply.interleaved(),
            [
                FrameKind::BootstrapBegin { .. },
                FrameKind::BootstrapChunk { .. },
                FrameKind::BootstrapReady { .. }
            ]
        ));
    }

    /// A hub pushes one uncorrelated ERROR per unreachable satellite before
    /// the `GET_STATE` ack; swallowing it hides a partial fleet view.
    #[test]
    fn satellite_degradation_error_is_not_swallowed_by_get_state() {
        let reply = request_against(vec![
            FrameKind::Error {
                request_id: None,
                code: ErrorCode::UnsupportedSatelliteRoute,
                message: "no satellite route to build-box".to_owned(),
            },
            ack(7),
        ]);
        assert!(
            matches!(reply.result(), CommandResult::Ok),
            "an uncorrelated ERROR is degradation, not the command's answer"
        );
        match reply.interleaved() {
            [FrameKind::Error { message, .. }] => {
                assert_eq!(message, "no satellite route to build-box");
            }
            other => panic!("degradation notice must survive, got {other:?}"),
        }
    }

    /// proto.md §9: an ERROR correlated to the request is its answer (e.g.
    /// `INVALID_COMMAND` for an unimplemented command), not a reason to hang.
    #[test]
    fn correlated_error_answers_the_request_instead_of_hanging_forever() {
        let reply = request_against(vec![FrameKind::Error {
            request_id: Some(7),
            code: ErrorCode::InvalidCommand,
            message: "command not supported by this server".to_owned(),
        }]);
        match reply.result() {
            CommandResult::Error { code, message } => {
                assert_eq!(*code, ErrorCode::InvalidCommand);
                assert_eq!(message, "command not supported by this server");
            }
            other => panic!("a correlated ERROR is this command's answer, got {other:?}"),
        }
        assert!(reply.interleaved().is_empty());
    }

    /// Pipelined requests share a connection: another request's ack is kept
    /// for its owner.
    #[test]
    fn another_requests_ack_is_kept_not_consumed() {
        let reply = request_against(vec![ack(99), ack(7)]);
        assert!(
            matches!(
                reply.interleaved(),
                [FrameKind::CommandResult { request_id: 99, .. }]
            ),
            "got {:?}",
            reply.interleaved()
        );
    }

    #[test]
    fn frames_are_returned_in_arrival_order() {
        let bell = FrameKind::Bell {
            terminal_id: ResourceId::local(1),
        };
        let mut script = bootstrap();
        script.extend([bell, ack(7)]);
        let reply = request_against(script);
        assert_eq!(reply.interleaved().len(), 4);
        assert!(matches!(
            reply.interleaved().last(),
            Some(FrameKind::Bell { .. })
        ));
    }

    /// One `GET_METADATA` round trip against `script`.
    fn metadata_against(script: Vec<FrameKind>) -> Reply<Answer<Option<Vec<u8>>>> {
        block_on(async {
            let mut client = scripted(script);
            tokio::time::timeout(
                WEDGE_TIMEOUT,
                client.request_metadata(7, Scope::Resource(ResourceId::local(1)), "k".to_owned()),
            )
            .await
            .expect("the read must resolve; a timeout here is the wedge itself")
            .expect("metadata reply")
        })
    }

    /// One `SPAWN_RESOURCE` round trip against `script`.
    fn spawn_against(script: Vec<FrameKind>) -> Reply<Answer<SpawnResult>> {
        block_on(async {
            let mut client = scripted(script);
            tokio::time::timeout(WEDGE_TIMEOUT, client.request_spawn(&spawn_frame(7)))
                .await
                .expect("the spawn must resolve; a timeout here is the wedge itself")
                .expect("spawn reply")
        })
    }

    fn metadata_value(request_id: u32, value: Option<&[u8]>) -> FrameKind {
        FrameKind::MetadataValue {
            request_id,
            value: value.map(<[u8]>::to_vec),
        }
    }

    fn refusal(request_id: u32) -> FrameKind {
        FrameKind::Error {
            request_id: Some(request_id),
            code: ErrorCode::PermissionDenied,
            message: "policy refused that scope".to_owned(),
        }
    }

    /// A correlated refusal answers a metadata read, and stays distinct from
    /// an unset key.
    #[test]
    fn metadata_refusal_answers_the_read_and_differs_from_unset() {
        let reply = metadata_against(vec![refusal(7)]);
        match reply.result() {
            Err(refused) => {
                assert_eq!(refused.code, ErrorCode::PermissionDenied);
                assert_eq!(refused.message, "policy refused that scope");
            }
            Ok(other) => panic!("a correlated ERROR is this read's answer, got {other:?}"),
        }
        let unset = metadata_against(vec![metadata_value(7, None)]);
        assert_eq!(unset.result().as_ref().ok(), Some(&None));
    }

    /// Users see refusals verbatim, so the code renders as words, not Debug.
    #[test]
    fn refusal_display_renders_the_code_as_words_not_debug() {
        let refused = Refusal {
            code: ErrorCode::TerminalNotFound,
            message: "pane @9 does not exist".to_owned(),
        };
        assert_eq!(
            refused.to_string(),
            "terminal not found: pane @9 does not exist"
        );
    }

    #[test]
    fn metadata_wait_keeps_an_uncorrelated_degradation_notice() {
        let notice = FrameKind::Error {
            request_id: None,
            code: ErrorCode::SatelliteUnreachable,
            message: "satellite build-box is unreachable".to_owned(),
        };
        let reply = metadata_against(vec![notice, metadata_value(7, Some(b"v"))]);
        assert_eq!(
            reply.result().as_ref().ok(),
            Some(&Some(b"v".to_vec())),
            "an uncorrelated ERROR is not this read's answer",
        );
        assert!(matches!(
            reply.interleaved(),
            [FrameKind::Error {
                request_id: None,
                ..
            }]
        ));
    }

    fn spawn_frame(request_id: u32) -> FrameKind {
        FrameKind::SpawnResource {
            request_id,
            group: phux_protocol::ids::GroupId::new(1),
            command: None,
            cwd: None,
            env: None,
            term: None,
            satellite: Some(phux_protocol::ids::SatelliteHost::new("build-box")),
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: None,
        }
    }

    /// A satellite may answer a relayed spawn with a correlated ERROR.
    #[test]
    fn spawn_refusal_answers_the_request_instead_of_hanging_forever() {
        let reply = spawn_against(vec![refusal(7)]);
        assert!(
            reply.result().is_err(),
            "a correlated ERROR is this spawn's answer, got {:?}",
            reply.result()
        );
    }

    #[test]
    fn spawn_wait_keeps_frames_pushed_ahead_of_the_reply() {
        let spawned = FrameKind::ResourceSpawned {
            request_id: 7,
            result: SpawnResult::Ok(ResourceId::local(3)),
        };
        let mut script = bootstrap();
        script.push(spawned);
        let reply = spawn_against(script);
        assert!(matches!(reply.result(), Ok(SpawnResult::Ok(_))));
        assert_eq!(reply.interleaved().len(), 3);
        assert!(matches!(
            reply.interleaved(),
            [
                FrameKind::BootstrapBegin { .. },
                FrameKind::BootstrapChunk { .. },
                FrameKind::BootstrapReady { .. }
            ]
        ));
    }

    /// The correlation id comes from the frame, so a non-spawn frame has no
    /// id to wait on and is refused.
    #[test]
    fn spawn_rejects_a_frame_that_is_not_a_spawn() {
        block_on(async {
            let (client_stream, _server) = UnixStream::pair().expect("pair");
            let mut client = Connection::from_stream(client_stream);
            assert!(matches!(
                client.request_spawn(&ack(1)).await,
                Err(AttachError::Protocol(_))
            ));
            drop(client);
        });
    }

    // --- SPEC §5 ERROR { FRAME_TOO_LARGE } -------------------------------

    /// Read the client's §5 goodbye off the far side of a `from_stream` pair.
    async fn recv_one_frame(peer: &mut UnixStream) -> FrameKind {
        let mut buf = BytesMut::new();
        loop {
            if let Some(frame) = decode_buffered(&mut buf, BootstrapLimits::default())
                .expect("peer reply must itself be a well-formed frame")
            {
                return frame;
            }
            let n = peer
                .read_buf(&mut buf)
                .await
                .expect("read ERROR{FRAME_TOO_LARGE}");
            assert!(n > 0, "peer closed before sending ERROR{{FRAME_TOO_LARGE}}");
        }
    }

    fn assert_framing_err(err: &AttachError, header: [u8; 4]) {
        let length = u32::from_be_bytes(header);
        assert!(
            matches!(
                err,
                AttachError::Framing(FramingError::LengthOutOfRange { length: got })
                    if *got == length
            ),
            "expected LengthOutOfRange({length}), got {err:?}"
        );
    }

    fn assert_frame_too_large(frame: &FrameKind, header: [u8; 4]) {
        let declared = u32::from_be_bytes(header);
        assert!(
            matches!(
                frame,
                FrameKind::Error {
                    request_id: None,
                    code: ErrorCode::FrameTooLarge,
                    message,
                } if message.contains(&declared.to_string())
            ),
            "expected ERROR{{FRAME_TOO_LARGE}} naming {declared}, got {frame:?}"
        );
        let FrameKind::Error { code, .. } = frame else {
            unreachable!("asserted above");
        };
        assert_eq!(code.as_wire(), 4, "FRAME_TOO_LARGE is wire value 4");
    }

    fn recv_answers_framing_violation(header: [u8; 4]) {
        block_on(async {
            let (client_stream, mut peer) = UnixStream::pair().expect("pair");
            let mut client = Connection::from_stream(client_stream);
            peer.write_all(&header).await.expect("write header");
            let err = client.recv().await.expect_err("framing violation");
            drop(client);
            assert_framing_err(&err, header);
            assert_frame_too_large(&recv_one_frame(&mut peer).await, header);
        });
    }

    /// SPEC §5: a peer receiving a length outside `1..=MAX_FRAME_LEN` MUST
    /// send `ERROR { FRAME_TOO_LARGE }` and close.
    #[test]
    fn recv_answers_a_framing_violation_with_frame_too_large() {
        recv_answers_framing_violation(0u32.to_be_bytes());
        recv_answers_framing_violation((MAX_FRAME_LEN + 1).to_be_bytes());
        recv_answers_framing_violation(u32::MAX.to_be_bytes());
    }

    /// Same obligation on the non-blocking drain path.
    #[test]
    fn try_recv_answers_a_framing_violation_with_frame_too_large() {
        block_on(async {
            let header = 0u32.to_be_bytes();
            let (client_stream, mut peer) = UnixStream::pair().expect("pair");
            let mut client = Connection::from_stream(client_stream);
            let mut bytes = framed(1);
            bytes.extend_from_slice(&header);
            peer.write_all(&bytes).await.expect("write burst");
            assert!(matches!(
                client.recv().await.expect("legal frame"),
                FrameKind::FrameAck { seq: 1, .. }
            ));
            let err = client.try_recv().expect_err("framing violation");
            drop(client);
            assert_framing_err(&err, header);
            assert_frame_too_large(&recv_one_frame(&mut peer).await, header);
        });
    }
}
