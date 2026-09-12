//! QUIC transport (`phux-y8v6`, [ADR-0007]).
//!
//! QUIC carries the **identical** length-prefixed phux frames the UDS and
//! WebSocket transports do (`docs/spec/proto.md` §5); only the byte stream
//! underneath differs. Each accepted connection opens exactly one
//! bidirectional QUIC stream — a reliable, ordered, octet stream, which is all
//! the wire contract requires — and frames flow over it exactly as over a Unix
//! socket. quinn's `RecvStream`/`SendStream` are the byte plumbing; the
//! `FrameReader`/`FrameWriter`/`Incoming` impls below are thin reframing.
//!
//! Why QUIC at all (ADR-0007): connection migration (roaming across networks),
//! 0-RTT resumption (sub-second reconnect), and mandatory TLS 1.3 — the
//! Mosh-class UX properties — without reimplementing SSP. quinn is the stack
//! ADR-0007 names; it rides the same rustls 0.23 + `ring` provider as the
//! `wss://` path (`transport::tls`), so QUIC adds no new crypto toolchain.
//!
//! **Auth.** TLS 1.3 is intrinsic to QUIC, so confidentiality is never
//! optional here. For *authentication* of routable (non-loopback) consumers
//! this transport mirrors the WebSocket bearer-token model (ADR-0031): the
//! dialer sends a length-prefixed token **preamble** as the very first bytes of
//! the bidi stream, validated against the [`TokenStore`](crate::auth) inside
//! `Incoming::accept` before any phux frame is read — the QUIC analogue of
//! the `Authorization: Bearer` header the WebSocket path validates during its
//! upgrade. The preamble is a transport-establishment detail, not a phux wire
//! frame, so the `FrameKind` codec is untouched. On a loopback (unauthenticated)
//! listener no preamble is expected and frames start immediately.
//!
//! [ADR-0007]: ../../../ADR/0007-mosh-class-transport-and-satellites.md

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::wire::framing;
use tracing::{debug, warn};

use super::{FrameReader, FrameWriter, Incoming, LENGTH_PREFIX};

/// Upper bound on the token preamble body, in bytes. Generous relative to the
/// fixed token length so a forward-compatible longer token still parses, but
/// small enough that a malformed/hostile length can never allocate much.
const MAX_TOKEN_PREAMBLE: usize = 256;

/// QUIC idle timeout. A connection with no traffic and no keep-alive for this
/// long is dropped; migration/roaming reconnects re-establish within it.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Keep-alive interval. Comfortably under [`IDLE_TIMEOUT`] so an attached but
/// quiet client (no keystrokes, no output) holds its connection open across
/// NATs rather than being reaped.
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// Output quinn may hold beyond what the congestion window lets it send.
///
/// quinn's default send window is 10 MB, bounded in practice by the peer's
/// 1.25 MB stream credit. On a link slower than the output (cmatrix over a
/// thin Wi-Fi or DERP path) `write_all` kept accepting until that credit was
/// spent, so the backlog — and the lag in front of every keystroke echo —
/// grew to seconds before the writer ever blocked. Backpressure never reached
/// the output pump, so the gap resync that skips a slow consumer to a fresh
/// checkpoint never fired.
///
/// Holding the window to the congestion window plus this slack is TCP's
/// `NOTSENT_LOWAT` rule: what is unsent stays small, so a slow link queues
/// about one round trip of output instead of megabytes. The slack keeps the
/// next write ready when an ack opens room, so the path stays window-limited
/// and the congestion window keeps growing on a fast link.
const UNSENT_SLACK: u64 = 16 * 1024;

/// Ceiling for the tracked send window: quinn's own default.
const MAX_SEND_WINDOW: u64 = 10 * 1024 * 1024;

/// The send window for a path whose congestion window is `cwnd` bytes.
fn send_window_for(cwnd: u64) -> u64 {
    cwnd.saturating_add(UNSENT_SLACK).min(MAX_SEND_WINDOW)
}

/// QUIC application close code for a connection refused at the auth preamble.
const AUTH_FAILED_CODE: u32 = 0x01;

/// Cap on client-opened bidi streams per QUIC connection (proto.md §4.2:
/// "per-connection stream count is capped on both legs").
///
/// Bounds panes-per-attach plus headroom for re-binds. quinn enforces it at
/// the transport: an over-cap opener stalls on `open_bi` rather than
/// consuming server tasks. 128 is far above any real attach (dozens of
/// panes) and far below task-exhaustion territory.
const MAX_CONCURRENT_BIDI_STREAMS: u64 = 128;

/// How long one admission step — handshake, first bidi stream, auth preamble —
/// may take before the connection is abandoned and the loop moves on.
///
/// [`IDLE_TIMEOUT`] does not cover this: it reaps an established connection
/// that goes quiet, but each `await` below happens *before* the connection is
/// admitted, and the accept loop is sequential. Un-timed, a peer that opens a
/// handshake and then says nothing parks the loop indefinitely, which stops
/// this listener accepting anyone else. Because the whole remote surface is
/// served by one accept path, a single unanswered UDP datagram to the QUIC
/// port takes wss down with it.
///
/// A real consumer completes each step immediately, so the bound only fires
/// on stalled peers. Mirrors `phux-relay`'s `PREAMBLE_DEADLINE`.
const ADMISSION_DEADLINE: Duration = Duration::from_secs(10);

/// A QUIC listener: a quinn [`Endpoint`](quinn::Endpoint) bound to a UDP
/// socket, optionally token-authenticated for routable consumers.
pub(crate) struct QuicListener {
    endpoint: quinn::Endpoint,
    tokens: Option<Arc<crate::auth::ReloadingTokenStore>>,
}

impl QuicListener {
    /// Bind a QUIC listener: build the (always-TLS) rustls config from the
    /// persisted cert + key, then open the endpoint. `tokens` selects the auth
    /// mode — `Some` requires a valid bearer-token preamble from each dialer
    /// (routable consumers, ADR-0031 parity with `wss://`); `None` is the
    /// loopback/dev path that expects no preamble. QUIC is TLS-encrypted in
    /// both modes (the protocol mandates it).
    #[allow(
        dead_code,
        reason = "kept as the compatibility constructor for transport tests"
    )]
    pub(crate) fn from_pem(
        addr: SocketAddr,
        cert_path: &std::path::Path,
        key_path: &std::path::Path,
        tokens: Option<Arc<crate::auth::ReloadingTokenStore>>,
    ) -> Result<Self, QuicBindError> {
        Self::from_pem_with_client_ca(addr, cert_path, key_path, tokens, None)
    }

    /// Bind a QUIC listener with optional workload-CA client verification.
    pub(crate) fn from_pem_with_client_ca(
        addr: SocketAddr,
        cert_path: &std::path::Path,
        key_path: &std::path::Path,
        tokens: Option<Arc<crate::auth::ReloadingTokenStore>>,
        client_ca_path: Option<&std::path::Path>,
    ) -> Result<Self, QuicBindError> {
        let tls =
            super::tls::quic_server_config_with_client_ca(cert_path, key_path, client_ca_path)?;
        Ok(Self {
            endpoint: build_endpoint(addr, tls)?,
            tokens,
        })
    }

    /// The local address the endpoint is bound to (for logging the OS-assigned
    /// port when binding to `:0`).
    pub(crate) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }
}

/// Errors from constructing a [`QuicListener`].
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

/// Assemble a quinn server [`Endpoint`](quinn::Endpoint) from a rustls config.
fn build_endpoint(
    addr: SocketAddr,
    tls: rustls::ServerConfig,
) -> Result<quinn::Endpoint, QuicBindError> {
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(crypto));

    let mut transport = quinn::TransportConfig::default();
    // `try_into` only fails on a duration that overflows QUIC's varint idle
    // encoding; our constants are well within range.
    if let Ok(idle) = IDLE_TIMEOUT.try_into() {
        transport.max_idle_timeout(Some(idle));
    }
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    if let Ok(cap) = quinn::VarInt::from_u64(MAX_CONCURRENT_BIDI_STREAMS) {
        transport.max_concurrent_bidi_streams(cap);
    }
    server_config.transport_config(Arc::new(transport));

    Ok(quinn::Endpoint::server(server_config, addr)?)
}

/// QUIC read half: reassembles length-prefixed frames off the bidi stream,
/// byte-for-byte the same framing as the UDS path.
pub(crate) struct QuicReader {
    recv: quinn::RecvStream,
}
impl QuicReader {
    /// Wrap one already-authenticated QUIC receive stream in phux framing.
    pub(crate) const fn from_stream(recv: quinn::RecvStream) -> Self {
        Self { recv }
    }
}

impl FrameReader for QuicReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        read_framed(&mut self.recv).await
    }
}

/// QUIC write half.
pub(crate) struct QuicWriter {
    send: quinn::SendStream,
    /// The connection `send` rides, read for its congestion window.
    conn: quinn::Connection,
    /// The send window last handed to quinn; `0` before the first write.
    send_window: u64,
}
impl QuicWriter {
    /// Wrap one already-authenticated QUIC send stream in phux framing.
    pub(crate) const fn from_stream(send: quinn::SendStream, conn: quinn::Connection) -> Self {
        Self {
            send,
            conn,
            send_window: 0,
        }
    }

    /// Hold quinn's send window to the congestion window plus
    /// [`UNSENT_SLACK`].
    ///
    /// Called before every write, so the bound follows the path as the
    /// congestion controller learns it. A write that finds the window full
    /// blocks, the per-client mailbox fills behind it, and the output pump
    /// falls behind the pane's broadcast — which is the lag the pump already
    /// answers with an in-band resync, skipping the client to a fresh
    /// checkpoint rather than replaying a backlog it cannot drain.
    fn track_congestion_window(&mut self) {
        let window = send_window_for(self.conn.stats().path.cwnd);
        if window != self.send_window {
            self.conn.set_send_window(window);
            self.send_window = window;
        }
    }

    /// Write all of `bytes`, re-tracking the window before every partial
    /// write.
    ///
    /// Not `write_all`: that would pin the window for the whole buffer, and a
    /// bootstrap batch runs to megabytes. Once the congestion window had grown
    /// past the pinned value quinn would be starved of unsent data, count the
    /// path as app-limited and stop growing the window — a fresh connection
    /// would crawl through its first screen at one pinned window per round
    /// trip instead of ramping up in slow start.
    async fn write_tracked(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            self.track_congestion_window();
            let written = self.send.write(bytes).await.map_err(io::Error::other)?;
            bytes = &bytes[written..];
        }
        Ok(())
    }
}

impl FrameWriter for QuicWriter {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.write_tracked(frame).await
    }

    /// One `write_all` for the whole batch, exactly as `UdsWriter` does.
    ///
    /// A QUIC bidi stream is a reliable ordered *byte* stream, not a datagram
    /// flow: `QuicReader` reassembles frames off it by the length prefix each
    /// one already carries, so the bytes it sees are identical either way.
    /// What changes is the number of `SendStream::write_all` futures a burst
    /// costs — a coalesced PTY burst of up to `MAX_WRITE_COALESCE` frames now
    /// pays one poll and one copy into quinn's stream buffer instead of 32,
    /// and quinn packs the result into full packets rather than being woken
    /// per frame. `ends` is unused for exactly that reason.
    async fn write_frames(&mut self, batch: &[u8], _ends: &[usize]) -> io::Result<()> {
        self.write_tracked(batch).await
    }

    #[allow(
        clippy::unused_async_trait_impl,
        reason = "FrameWriter requires an async close operation, while Quinn's finish is synchronous"
    )]
    async fn close(&mut self) -> io::Result<()> {
        self.send.finish().map_err(io::Error::other)
    }
}

impl Incoming for QuicListener {
    type Reader = QuicMuxReader;
    type Writer = QuicWriter;

    fn transport_type(&self) -> TransportType {
        TransportType::Quic
    }

    async fn accept(
        &self,
    ) -> io::Result<(QuicMuxReader, QuicWriter, crate::auth::ConnectionIdentity)> {
        // One QUIC endpoint multiplexes many connections; a single bad
        // handshake or refused token must not tear the listener down, so
        // per-connection failures `continue` (logged) and only an endpoint
        // closure ends the loop. This is the multiplexed-endpoint analogue of
        // the per-`accept()` TCP loop the WebSocket path runs.
        loop {
            let incoming = self.endpoint.accept().await.ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotConnected, "quic endpoint closed")
            })?;
            let remote = incoming.remote_address();

            let conn = match tokio::time::timeout(ADMISSION_DEADLINE, incoming).await {
                Ok(Ok(conn)) => conn,
                Ok(Err(err)) => {
                    debug!(%remote, error = %err, "quic handshake failed");
                    continue;
                }
                Err(_) => {
                    debug!(%remote, "quic handshake timed out");
                    continue;
                }
            };

            // The consumer opens one bidi stream and immediately writes its
            // first bytes (token preamble, then frames), so `accept_bi`
            // resolves promptly.
            let (send, mut recv) =
                match tokio::time::timeout(ADMISSION_DEADLINE, conn.accept_bi()).await {
                    Ok(Ok(pair)) => pair,
                    Ok(Err(err)) => {
                        debug!(%remote, error = %err, "quic stream accept failed");
                        continue;
                    }
                    Err(_) => {
                        debug!(%remote, "quic stream accept timed out");
                        conn.close(AUTH_FAILED_CODE.into(), b"stream timeout");
                        continue;
                    }
                };

            let credential = match &self.tokens {
                Some(store) => {
                    let preamble = tokio::time::timeout(
                        ADMISSION_DEADLINE,
                        authorize_preamble(&mut recv, store),
                    )
                    .await;
                    let Ok(Some(credential)) = preamble else {
                        if preamble.is_err() {
                            debug!(%remote, "quic auth preamble timed out");
                        } else {
                            warn!(%remote, "quic consumer refused: missing or invalid token");
                        }
                        conn.close(AUTH_FAILED_CODE.into(), b"unauthorized");
                        continue;
                    };
                    Some(credential)
                }
                None => None,
            };

            let peer_identity = PeerIdentity {
                uid: 0,
                pid: None,
                exe_path: None,
                mcp_host_key: credential.as_ref().map(|credential| credential.id.clone()),
                transport: TransportType::Quic,
                source_addr: Some(remote.ip()),
            };

            return Ok((
                QuicMuxReader::new(recv, conn.clone()),
                QuicWriter::from_stream(send, conn),
                crate::auth::ConnectionIdentity {
                    peer: peer_identity,
                    credential,
                    ssh_origin: None,
                },
            ));
        }
    }

    fn kind(&self) -> &'static str {
        "quic"
    }
}

/// Read the token preamble (`len: u32 BE` + `len` token bytes) off the stream
/// and verify it against the store. Returns the stable credential id on
/// success, or `None` on a missing, oversized, malformed, or unrecognized token.
pub(crate) async fn authorize_preamble(
    recv: &mut quinn::RecvStream,
    store: &crate::auth::ReloadingTokenStore,
) -> Option<crate::auth::AuthenticatedCredential> {
    let mut len_buf = [0u8; LENGTH_PREFIX];
    if !read_exact_quic(recv, &mut len_buf).await.ok()? {
        return None;
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > MAX_TOKEN_PREAMBLE {
        return None;
    }
    let mut token = vec![0u8; len];
    if !read_exact_quic(recv, &mut token).await.ok()? {
        return None;
    }
    store.authenticate(&token)
}

/// Depth of the mux's merged frame channel: control plus every bound
/// Terminal stream funnel through it. Small on purpose — a stalled client
/// task stalls the connection, exactly as a stalled direct read does today;
/// per-stream isolation lives in QUIC flow control and the per-stream
/// writer queues, not here.
const MUX_FRAME_CHANNEL: usize = 64;

/// Depth of the Terminal-stream event channel. Binds and stream ends are
/// infrequent (one per attach/detach); the accept loop awaits sends, so a
/// full channel only ever reflects a client task that stopped polling —
/// which is connection teardown by another name.
const MUX_EVENT_CHANNEL: usize = 32;

/// QUIC application error code resetting a stream whose `STREAM_BIND` was
/// malformed or never completed within the admission deadline.
const BIND_REFUSED_CODE: u32 = 0x10;

/// Refuse a bound Terminal stream the client task rejected (unknown or
/// unauthorized Terminal, failed bootstrap): reset it unread per proto.md
/// §4.2. The uncorrelated `ERROR` on control carries the reason; the reset
/// carries none.
pub(crate) fn refuse_terminal_stream(mut send: quinn::SendStream) {
    let _ = send.reset(quinn::VarInt::from_u32(BIND_REFUSED_CODE));
}

/// A Terminal stream's lifecycle event, delivered to the client task after
/// it takes the mux's event channel (proto.md §4.2, ADR-0113).
#[derive(Debug)]
pub(crate) enum QuicStreamEvent {
    /// A well-formed `STREAM_BIND` arrived. The send half rides along so
    /// the client task can hand it to a per-stream writer with no shared
    /// table between the mux and the writer.
    Bound {
        /// The Terminal whose §4 frames ride this QUIC stream.
        terminal_id: phux_protocol::ids::ResourceId,
        /// The app-level stream generation this stream opens under.
        stream_id: phux_protocol::ids::StreamId,
        /// This stream's send half, for the per-stream writer.
        send: quinn::SendStream,
        /// The connection the stream rides, for congestion tracking.
        conn: quinn::Connection,
    },
    /// A bound stream ended (client `finish` or reset): the detach signal.
    /// The client task unsubscribes the pump exactly as for an explicit
    /// `DETACH_RESOURCE`.
    Ended {
        /// The Terminal the ended stream carried.
        terminal_id: phux_protocol::ids::ResourceId,
        /// The generation the ended stream opened under.
        stream_id: phux_protocol::ids::StreamId,
    },
}

/// QUIC read half with multi-stream upgrade (`docs/spec/proto.md` §4.2).
///
/// Pre-upgrade this is byte-for-byte the old `QuicReader`: frames come off
/// the control stream directly. The client task calls
/// [`FrameReader::take_stream_events`] once, after HELLO negotiates
/// `QUIC_STREAMS`; from then on the control stream and every bound
/// Terminal stream pump into one merged frame channel, and binds/ends
/// arrive on the event channel. Dropping the reader aborts the mux tasks.
pub(crate) struct QuicMuxReader {
    control: Option<QuicReader>,
    conn: quinn::Connection,
    frames_rx: Option<tokio::sync::mpsc::Receiver<BytesMut>>,
    control_open: bool,
    control_done: Option<tokio::sync::oneshot::Receiver<()>>,
    tasks: tokio::task::JoinSet<()>,
}

impl QuicMuxReader {
    /// Wrap the just-accepted control stream. Single-stream behavior until
    /// [`FrameReader::take_stream_events`] upgrades the connection.
    pub(crate) fn new(recv: quinn::RecvStream, conn: quinn::Connection) -> Self {
        Self {
            control: Some(QuicReader::from_stream(recv)),
            conn,
            frames_rx: None,
            control_open: true,
            control_done: None,
            tasks: tokio::task::JoinSet::new(),
        }
    }
}

impl FrameReader for QuicMuxReader {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        let Some(rx) = self.frames_rx.as_mut() else {
            // Pre-upgrade: the control stream, directly. `frames_rx` is
            // only set by the upgrade below, which takes `control` with
            // it — so `control` is present exactly when this branch runs.
            let Some(reader) = self.control.as_mut() else {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "quic mux upgraded without its control stream",
                ));
            };
            return reader.read_frame().await;
        };
        if !self.control_open {
            return Ok(None);
        }
        tokio::select! {
            biased;
            done = async {
                match self.control_done.as_mut() {
                    Some(rx) => rx.await.map_err(|_| ()),
                    None => core::future::pending().await,
                }
            } => {
                // Control end is connection end, even with live Terminal
                // streams: without control there is no HELLO/COMMAND/DETACH
                // channel, so the attach cannot continue.
                let _ = done;
                self.control_open = false;
                Ok(None)
            }
            frame = rx.recv() => Ok(frame),
        }
    }

    fn take_stream_events(&mut self) -> Option<tokio::sync::mpsc::Receiver<QuicStreamEvent>> {
        if self.frames_rx.is_some() {
            return None;
        }
        let (frames_tx, frames_rx) = tokio::sync::mpsc::channel(MUX_FRAME_CHANNEL);
        let (events_tx, events_rx) = tokio::sync::mpsc::channel(MUX_EVENT_CHANNEL);
        let (control_done_tx, control_done_rx) = tokio::sync::oneshot::channel();
        let control = self.control.take()?;
        let conn = self.conn.clone();
        // Control pump: the pre-upgrade direct read, moved into a task so
        // post-upgrade reads merge with Terminal streams. Its end is the
        // connection's end (see `read_frame`).
        let control_frames_tx = frames_tx.clone();
        self.tasks.spawn(async move {
            let mut control = control;
            while let Ok(Some(frame)) = control.read_frame().await {
                if control_frames_tx.send(frame).await.is_err() {
                    return;
                }
            }
            drop(control_done_tx);
        });
        // Stream-accept loop: binds only. The client task owns admission —
        // the mux forwards every well-formed bind and resets the rest.
        self.tasks.spawn(async move {
            accept_terminal_streams(conn, frames_tx, events_tx).await;
        });
        self.frames_rx = Some(frames_rx);
        self.control_done = Some(control_done_rx);
        Some(events_rx)
    }
}

/// Accept client-opened Terminal streams for one upgraded connection.
///
/// Each stream must open with a well-formed `STREAM_BIND` within
/// [`ADMISSION_DEADLINE`]; anything else is reset unread. Admission itself
/// (attached? authorized?) belongs to the client task, which holds the
/// send half from the `Bound` event and resets the stream on refusal.
///
/// Accepted streams pump their frames into the mux's merged frame channel;
/// when a stream ends the pump emits `Ended` so the client task can
/// unsubscribe it. Pumps are detached tasks with channel-tied lifecycles:
/// every await is on the stream or a channel send, so connection teardown
/// (which closes both) always ends them.
async fn accept_terminal_streams(
    conn: quinn::Connection,
    frames_tx: tokio::sync::mpsc::Sender<BytesMut>,
    events_tx: tokio::sync::mpsc::Sender<QuicStreamEvent>,
) {
    loop {
        let Ok((mut send, mut recv)) = conn.accept_bi().await else {
            // Connection closed: the mux tasks end with it.
            return;
        };
        let bind = tokio::time::timeout(ADMISSION_DEADLINE, read_stream_bind(&mut recv)).await;
        let Some(bind) = bind.ok().flatten() else {
            debug!("quic terminal stream refused: no well-formed STREAM_BIND within deadline");
            let _ = send.reset(quinn::VarInt::from_u32(BIND_REFUSED_CODE));
            let _ = recv.stop(quinn::VarInt::from_u32(BIND_REFUSED_CODE));
            continue;
        };
        let event = QuicStreamEvent::Bound {
            terminal_id: bind.terminal_id.clone(),
            stream_id: bind.stream_id,
            send,
            conn: conn.clone(),
        };
        if events_tx.send(event).await.is_err() {
            return;
        }
        // Per-stream pump, so one stream's stall never parks the accept
        // loop. Detached by design (see above); plain `spawn` (not
        // `spawn_local`) because every future here is `Send` and the
        // transport must not assume the caller's runtime shape.
        let pump_frames = frames_tx.clone();
        let pump_events = events_tx.clone();
        let terminal_id = bind.terminal_id;
        let stream_id = bind.stream_id;
        tokio::task::spawn(async move {
            pump_terminal_stream(recv, terminal_id, stream_id, pump_frames, pump_events).await;
        });
    }
}

/// Pump one bound Terminal stream's frames into the merged channel until the
/// stream ends, then emit `Ended`.
async fn pump_terminal_stream(
    mut recv: quinn::RecvStream,
    terminal_id: phux_protocol::ids::ResourceId,
    stream_id: phux_protocol::ids::StreamId,
    frames_tx: tokio::sync::mpsc::Sender<BytesMut>,
    events_tx: tokio::sync::mpsc::Sender<QuicStreamEvent>,
) {
    loop {
        let Ok(Some(frame)) = read_framed(&mut recv).await else {
            // Clean finish, reset, or transport error: the stream is over
            // either way. A mid-frame truncation also ends here rather than
            // poisoning the merged channel — the generation is bad and the
            // tombstone machinery (L1 §4.6) owns recovery once the client
            // re-binds.
            let _ = events_tx
                .send(QuicStreamEvent::Ended {
                    terminal_id,
                    stream_id,
                })
                .await;
            return;
        };
        if frames_tx.send(frame).await.is_err() {
            return;
        }
    }
}

/// Read one `STREAM_BIND` header off a fresh Terminal stream: `len: u32 BE`
/// followed by exactly that many body bytes.
async fn read_stream_bind(
    recv: &mut quinn::RecvStream,
) -> Option<phux_protocol::wire::stream_bind::StreamBind> {
    let mut len_buf = [0u8; 4];
    read_exact_quic(recv, &mut len_buf).await.ok()?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > phux_protocol::wire::stream_bind::MAX_STREAM_BIND_BYTES {
        return None;
    }
    let mut body = vec![0u8; len];
    read_exact_quic(recv, &mut body).await.ok()?;
    let mut framed = Vec::with_capacity(4 + len);
    framed.extend_from_slice(&len_buf);
    framed.extend_from_slice(&body);
    phux_protocol::wire::stream_bind::decode(&framed)
        .ok()
        .map(|(bind, _)| bind)
}

/// Fill `buf` from the QUIC stream. Returns `Ok(true)` when `buf` is filled,
/// `Ok(false)` on a clean stream finish before any byte was read (end of the
/// connection at a frame boundary), and `Err` on a partial-then-finished read
/// (a truncated frame) or a transport error.
async fn read_exact_quic(recv: &mut quinn::RecvStream, buf: &mut [u8]) -> io::Result<bool> {
    match recv.read_exact(buf).await {
        Ok(()) => Ok(true),
        // Zero bytes before the stream finished is a clean EOF at a frame
        // boundary; any other shortfall is a truncated frame.
        Err(quinn::ReadExactError::FinishedEarly(0)) => Ok(false),
        Err(err) => Err(io::Error::other(err)),
    }
}

/// Read one length-prefixed frame off a QUIC receive stream: the framing half
/// of [`FrameReader::read_frame`], shared by the control pump and every
/// Terminal-stream pump the mux spawns.
async fn read_framed(recv: &mut quinn::RecvStream) -> io::Result<Option<BytesMut>> {
    let mut header = [0u8; LENGTH_PREFIX];
    if !read_exact_quic(recv, &mut header).await? {
        // Clean stream finish at a frame boundary: end of connection.
        return Ok(None);
    }
    let mut framed = framing::frame_buffer(header)?;
    if !read_exact_quic(recv, &mut framed[LENGTH_PREFIX..]).await? {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "stream finished mid-frame",
        ));
    }
    Ok(Some(framed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use super::super::tls::{QUIC_ALPN, ensure_self_signed};

    const TEST_TOKEN: [u8; crate::auth::TOKEN_LEN] = [0x11; crate::auth::TOKEN_LEN];
    /// One complete framed message: 4-byte length prefix (body = 3) + body.
    const FRAME: [u8; 7] = [0, 0, 0, 3, 0xde, 0xad, 0xbe];

    #[test]
    fn send_window_is_the_congestion_window_plus_unsent_slack() {
        // Even a collapsed path keeps the slack, so the writer always has
        // somewhere to put its next batch.
        assert_eq!(send_window_for(0), UNSENT_SLACK);
        // In range, only the slack sits beyond what the path may send.
        assert_eq!(send_window_for(14_720), 14_720 + UNSENT_SLACK);
        assert_eq!(send_window_for(100_000), 100_000 + UNSENT_SLACK);
        // A huge window is capped at quinn's own default, without overflow.
        assert_eq!(send_window_for(MAX_SEND_WINDOW), MAX_SEND_WINDOW);
        assert_eq!(send_window_for(u64::MAX), MAX_SEND_WINDOW);
    }

    /// A self-signed cert + key in a fresh tempdir, kept alive for the test.
    fn cert_pair() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        ensure_self_signed(&cert, &key).unwrap();
        (dir, cert, key)
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

    /// A quinn client endpoint that offers the phux ALPN and trusts the
    /// listener's self-signed cert.
    ///
    /// QUIC mandates ALPN + TLS, so the handshake is exercised end to end; the
    /// self-signed leaf is trusted blindly rather than pinned, the dialer's
    /// fingerprint-pinning being out of scope for the listener under test.
    /// That policy comes from [`phux_dial::tls::client_config`] — the same
    /// builder production consumers dial through — rather than from a
    /// `ServerCertVerifier` hand-rolled here, which is what this test used to
    /// carry. A test that re-implements the trust policy is a test that can
    /// quietly disagree with it.
    fn client_endpoint() -> quinn::Endpoint {
        let crypto =
            phux_dial::tls::client_config(&phux_dial::CertTrust::SkipVerify, Some(QUIC_ALPN))
                .unwrap();
        let client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
        ));
        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(client_config);
        endpoint
    }

    /// Frame a token as the auth preamble: `len: u32 BE` + token bytes.
    fn token_preamble(token: &[u8]) -> Vec<u8> {
        let mut buf = (u32::try_from(token.len()).unwrap()).to_be_bytes().to_vec();
        buf.extend_from_slice(token);
        buf
    }

    #[tokio::test]
    async fn round_trips_a_frame_unauthenticated() {
        let (_dir, cert, key) = cert_pair();
        let listener =
            QuicListener::from_pem("127.0.0.1:0".parse().unwrap(), &cert, &key, None).unwrap();
        let addr = listener.local_addr().unwrap();

        let server = async {
            let (mut reader, _writer, peer) = listener.accept().await.unwrap();
            let got = reader.read_frame().await.unwrap();
            (got, peer)
        };
        let client = async {
            let endpoint = client_endpoint();
            let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
            let (mut send, _recv) = conn.open_bi().await.unwrap();
            send.write_all(&FRAME).await.unwrap();
            // Hold the connection open until the server has read the frame.
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        let ((got, peer), ()) = tokio::join!(server, client);
        assert_eq!(got.unwrap().as_ref(), &FRAME, "frame round-trips over QUIC");
        assert_eq!(peer.transport, TransportType::Quic);
        assert!(
            peer.mcp_host_key.is_none(),
            "an unauthenticated loopback peer carries no device id"
        );
    }

    #[tokio::test]
    async fn valid_token_preamble_authenticates_and_round_trips() {
        let (_dir, cert, key) = cert_pair();
        let (_tok_file, store) = token_store();
        let listener =
            QuicListener::from_pem("127.0.0.1:0".parse().unwrap(), &cert, &key, Some(store))
                .unwrap();
        let addr = listener.local_addr().unwrap();

        let server = async {
            let (mut reader, _writer, peer) = listener.accept().await.unwrap();
            let got = reader.read_frame().await.unwrap();
            (got, peer)
        };
        let client = async {
            let endpoint = client_endpoint();
            let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
            let (mut send, _recv) = conn.open_bi().await.unwrap();
            send.write_all(&token_preamble(&TEST_TOKEN)).await.unwrap();
            send.write_all(&FRAME).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        let ((got, peer), ()) = tokio::join!(server, client);
        assert_eq!(
            got.unwrap().as_ref(),
            &FRAME,
            "frame round-trips after auth"
        );
        assert_eq!(peer.transport, TransportType::Quic);
        assert!(
            peer.mcp_host_key.is_some(),
            "an authenticated remote peer is non-anonymous"
        );
    }

    #[tokio::test]
    async fn invalid_token_preamble_is_refused() {
        let (_dir, cert, key) = cert_pair();
        let (_tok_file, store) = token_store();
        let listener =
            QuicListener::from_pem("127.0.0.1:0".parse().unwrap(), &cert, &key, Some(store))
                .unwrap();
        let addr = listener.local_addr().unwrap();

        // The listener loops internally on a refused connection (it serves a
        // multiplexed endpoint), so drive `accept` on a detached task and
        // assert the refusal from the *client* side: the server closes the
        // connection with the auth error code before any frame is read.
        let server = tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let endpoint = client_endpoint();
        let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
        let (mut send, _recv) = conn.open_bi().await.unwrap();
        let wrong = [0x22u8; crate::auth::TOKEN_LEN];
        let _ = send.write_all(&token_preamble(&wrong)).await;

        let closed = tokio::time::timeout(Duration::from_secs(5), conn.closed())
            .await
            .expect("server must refuse promptly, not hang");
        match closed {
            quinn::ConnectionError::ApplicationClosed(close) => {
                assert_eq!(u64::from(AUTH_FAILED_CODE), close.error_code.into_inner());
            }
            other => panic!("expected application close on auth failure, got {other:?}"),
        }
        server.abort();
    }

    /// Encode a `STREAM_BIND` header for tests (the production encoder is
    /// `phux_protocol::wire::stream_bind::encode`, exercised here verbatim).
    fn stream_bind_bytes(terminal: u32, stream: u64) -> Vec<u8> {
        use phux_protocol::ids::{ResourceId, StreamId};
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

    #[tokio::test]
    async fn mux_upgrades_and_merges_terminal_streams() {
        let (_dir, cert, key) = cert_pair();
        let listener =
            QuicListener::from_pem("127.0.0.1:0".parse().unwrap(), &cert, &key, None).unwrap();
        let addr = listener.local_addr().unwrap();

        let server = async {
            let (mut reader, _writer, _) = listener.accept().await.unwrap();
            // Pre-upgrade: the control stream, directly.
            let control = reader.read_frame().await.unwrap().unwrap();
            // Upgrade: takes the event channel; twice returns None.
            let mut events = reader.take_stream_events().expect("upgrade once");
            assert!(reader.take_stream_events().is_none(), "upgrade is one-shot");
            // One frame off the terminal stream, merged into the same read.
            let merged = tokio::time::timeout(Duration::from_secs(5), reader.read_frame())
                .await
                .expect("merged frame arrives")
                .unwrap()
                .unwrap();
            // The bind event names the terminal and generation.
            let bound = tokio::time::timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("bind event arrives")
                .expect("event channel open");
            // A clean client finish surfaces as the detach signal.
            let ended = tokio::time::timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("end event arrives")
                .expect("event channel open");
            (control, merged, bound, ended)
        };
        let client = async {
            let endpoint = client_endpoint();
            let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
            let (mut control_send, _recv) = conn.open_bi().await.unwrap();
            control_send.write_all(&FRAME).await.unwrap();
            // Give the server a chance to read the control frame pre-upgrade
            // and arm the mux before the terminal stream opens.
            tokio::time::sleep(Duration::from_millis(200)).await;
            let (mut term_send, _term_recv) = conn.open_bi().await.unwrap();
            term_send.write_all(&stream_bind_bytes(7, 1)).await.unwrap();
            term_send.write_all(&FRAME).await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            term_send.finish().unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        };

        let ((control, merged, bound, ended), ()) = tokio::join!(server, client);
        assert_eq!(control.as_ref(), &FRAME, "control frame pre-upgrade");
        assert_eq!(
            merged.as_ref(),
            &FRAME,
            "terminal frame merged post-upgrade"
        );
        match bound {
            QuicStreamEvent::Bound {
                terminal_id,
                stream_id,
                ..
            } => {
                assert_eq!(terminal_id, phux_protocol::ids::ResourceId::local(7));
                assert_eq!(stream_id.get(), 1);
            }
            ended @ QuicStreamEvent::Ended { .. } => {
                panic!("expected Bound, got {ended:?}")
            }
        }
        match ended {
            QuicStreamEvent::Ended {
                terminal_id,
                stream_id,
            } => {
                assert_eq!(terminal_id, phux_protocol::ids::ResourceId::local(7));
                assert_eq!(stream_id.get(), 1);
            }
            bound @ QuicStreamEvent::Bound { .. } => {
                panic!("expected Ended, got {bound:?}")
            }
        }
    }

    #[tokio::test]
    async fn mux_resets_a_stream_with_no_well_formed_bind() {
        let (_dir, cert, key) = cert_pair();
        let listener =
            QuicListener::from_pem("127.0.0.1:0".parse().unwrap(), &cert, &key, None).unwrap();
        let addr = listener.local_addr().unwrap();

        let server = async {
            let (mut reader, _writer, _) = listener.accept().await.unwrap();
            // Drain the control frame, then upgrade.
            let _ = reader.read_frame().await.unwrap().unwrap();
            let mut events = reader.take_stream_events().expect("upgrade once");
            // No bind event may arrive for the garbage stream; the accept
            // loop stays alive for later well-formed binds (asserted by a
            // short quiet window, then the test ends).
            let quiet = tokio::time::timeout(Duration::from_millis(300), events.recv()).await;
            assert!(
                quiet.is_err(),
                "malformed bind emits no event, got {quiet:?}"
            );
        };
        let client = async {
            let endpoint = client_endpoint();
            let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
            let (mut control_send, _recv) = conn.open_bi().await.unwrap();
            control_send.write_all(&FRAME).await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            let (mut bad_send, mut bad_recv) = conn.open_bi().await.unwrap();
            bad_send.write_all(b"not a bind header").await.unwrap();
            // The server resets the stream: the client's read side errors.
            let mut buf = [0u8; 8];
            let err = tokio::time::timeout(Duration::from_secs(5), bad_recv.read(&mut buf)).await;
            assert!(
                matches!(err, Ok(Err(_))),
                "reset stream errors the reader, got {err:?}"
            );
            // Stay alive past the server's quiet window: returning here
            // would tear the connection down, closing the event channel
            // the server is asserting silence on.
            tokio::time::sleep(Duration::from_millis(600)).await;
            let _ = bad_send;
        };

        tokio::join!(server, client);
    }
}
