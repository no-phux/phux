//! Framed I/O over one established link: the SPEC §5 frame stream every
//! client lane reads and writes.
//!
//! The reconnecting driver pumps an [`Io`] into the control plane; a
//! one-shot [`crate::session::Session`] reads and writes it directly. Both
//! get the same framing, send budgets, WebSocket liveness, and typed
//! [`LinkError`]s, which each words in its own vocabulary.

#![allow(
    clippy::redundant_pub_crate,
    reason = "crate-internal: the session module reads links; `unreachable_pub` forbids plain `pub`"
)]

use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::FutureExt;
use phux_dial::DialError;
use phux_dial::ws::{WsActivity, WsReader, WsWriter, recv_activity_alive};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::framing::{self, FramingError};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::control::encode;
use crate::dial::dial_message;

const MAX_INBOUND_BATCH: usize = 256;

/// How long a send may go without the kernel accepting another chunk.
///
/// A shut window trips this. A slow link that keeps taking chunks does not:
/// each accepted chunk starts a fresh budget.
pub(super) const SEND_BUDGET: Duration = Duration::from_secs(15);

/// Longest a single WebSocket message may take. The message is one send, so
/// the budget grows by [`SEND_BUDGET`] per [`SEND_STALL_CHUNK`] and stops here.
/// A shut window on a small frame still dies at [`SEND_BUDGET`].
const SEND_BUDGET_CEILING: Duration = Duration::from_secs(60);

/// Bytes the kernel must accept before [`SEND_BUDGET`] resets. Smaller than
/// a typical socket buffer, so a live link finishes a chunk and a shut
/// window does not.
const SEND_STALL_CHUNK: usize = 16 * 1024;

/// The application-close reason a client's QUIC teardown sends.
const CLOSE_REASON: &[u8] = b"session closed";

/// Why reading or writing an established link failed.
///
/// Typed, so the reconnecting driver can word it for an operator
/// ([`Self::describe`]) and a one-shot session can keep the OS error kind
/// its callers classify a gone peer on.
#[derive(Debug)]
pub(crate) enum LinkError {
    /// A read failed at the OS or QUIC stream layer.
    Read(std::io::Error),
    /// A write failed at the OS or QUIC stream layer.
    Write(std::io::Error),
    /// A write made no progress for [`SEND_BUDGET`].
    Stalled,
    /// The peer broke SPEC §5 framing.
    Framing(FramingError),
    /// The WebSocket lane failed (I/O, protocol, or liveness).
    Ws(DialError),
    /// The QUIC connection closed under a finished stream, for this reason.
    Closed(DialError),
}

impl LinkError {
    /// The driver's operator-facing wording, naming the host.
    pub(crate) fn describe(&self, name: &str) -> String {
        match self {
            Self::Read(error) => format!("{name}: the connection was lost: {error}"),
            Self::Write(error) => format!("{name}: send failed: {error}"),
            Self::Stalled => "send stalled".to_owned(),
            Self::Framing(error) => format!("{name} sent a malformed frame: {error}"),
            Self::Ws(error) | Self::Closed(error) => dial_message(name, error),
        }
    }
}

/// A QUIC link's connection state, closed when the link is dropped so the
/// server reaps the consumer at once rather than at its idle timeout.
#[derive(Debug)]
pub(crate) struct QuicLink {
    endpoint: quinn::Endpoint,
    connection: quinn::Connection,
    /// Whether the connection may carry per-Terminal streams: a plain QUIC
    /// stream, not an end-to-end session a relay splices (proto.md §4.2,
    /// "Relay": such a consumer stays on the single-stream shape).
    terminal_streams: bool,
}

impl QuicLink {
    fn close(&self) {
        self.connection
            .close(quinn::VarInt::from_u32(0), CLOSE_REASON);
    }
}

impl Drop for QuicLink {
    fn drop(&mut self) {
        self.close();
    }
}

/// One established lane, read and written as SPEC section 5 frames.
pub(crate) enum Io {
    Stream {
        reader: Box<dyn AsyncRead + Unpin + Send + Sync>,
        writer: Box<dyn AsyncWrite + Unpin + Send + Sync>,
        pending: BytesMut,
        /// Kept so the QUIC endpoint's driver outlives the connection and a
        /// clean close can be issued.
        quic: Option<QuicLink>,
        /// The server's pid from Unix-socket peer credentials.
        peer_pid: Option<i32>,
    },
    Ws {
        reader: WsReader,
        writer: WsWriter,
        deferred_error: Option<LinkError>,
    },
}

impl std::fmt::Debug for Io {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Stream { quic: None, .. } => "Io::Stream",
            Self::Stream { quic: Some(_), .. } => "Io::Stream(quic)",
            Self::Ws { .. } => "Io::Ws",
        })
    }
}

impl Io {
    /// A byte-stream lane over `reader` and `writer`.
    #[cfg(test)]
    pub(crate) fn stream(
        reader: Box<dyn AsyncRead + Unpin + Send + Sync>,
        writer: Box<dyn AsyncWrite + Unpin + Send + Sync>,
    ) -> Self {
        Self::framed(reader, writer, None, None)
    }

    /// A lane over a connected Unix socket, its peer pid captured first
    /// (credentials are only readable before the split).
    pub(crate) fn from_unix_stream(stream: tokio::net::UnixStream) -> Self {
        let peer_pid = stream.peer_cred().ok().and_then(|cred| cred.pid());
        let (reader, writer) = stream.into_split();
        Self::framed(Box::new(reader), Box::new(writer), None, peer_pid)
    }

    /// A lane over a dialed QUIC stream. Only a plain stream may later carry
    /// per-Terminal streams; an end-to-end session a relay splices may not.
    pub(crate) fn quic(
        endpoint: quinn::Endpoint,
        connection: quinn::Connection,
        stream: phux_dial::quic::DialedStream,
    ) -> Self {
        let terminal_streams = matches!(stream, phux_dial::quic::DialedStream::Plain { .. });
        let (reader, writer) = crate::tunnel::host_halves(stream);
        let link = QuicLink {
            endpoint,
            connection,
            terminal_streams,
        };
        Self::framed(reader, writer, Some(link), None)
    }

    fn framed(
        reader: Box<dyn AsyncRead + Unpin + Send + Sync>,
        writer: Box<dyn AsyncWrite + Unpin + Send + Sync>,
        quic: Option<QuicLink>,
        peer_pid: Option<i32>,
    ) -> Self {
        Self::Stream {
            reader,
            writer,
            pending: BytesMut::with_capacity(64 * 1024),
            quic,
            peer_pid,
        }
    }

    /// A WebSocket lane over an upgraded connection.
    pub(crate) fn ws(ws: phux_dial::ws::Ws) -> Self {
        let (tx, rx) = futures_util::StreamExt::split(ws);
        Self::Ws {
            reader: WsReader::new(rx),
            writer: WsWriter { tx },
            deferred_error: None,
        }
    }

    /// The server's pid from Unix-socket peer credentials; `None` on remote
    /// lanes or when the platform omits it.
    pub(crate) const fn peer_pid(&self) -> Option<i32> {
        match self {
            Self::Stream { peer_pid, .. } => *peer_pid,
            Self::Ws { .. } => None,
        }
    }

    /// The QUIC connection per-Terminal streams may open on, when this lane
    /// can carry them (see [`QuicLink`]).
    pub(crate) fn terminal_stream_connection(&self) -> Option<quinn::Connection> {
        match self {
            Self::Stream {
                quic: Some(link), ..
            } if link.terminal_streams => Some(link.connection.clone()),
            _ => None,
        }
    }

    /// Whether writes can be batched into one: a byte stream that is not
    /// QUIC (QUIC coalesces in its own send buffer, and WebSocket needs one
    /// message per frame).
    pub(crate) const fn batches_writes(&self) -> bool {
        matches!(self, Self::Stream { quic: None, .. })
    }

    /// The next bounded transport batch, or `Ok(None)` on a clean close.
    /// Cancel-safe: partial bytes stay buffered.
    pub(crate) async fn read_frames(&mut self) -> Result<Option<Vec<Bytes>>, LinkError> {
        match self {
            Self::Stream {
                reader,
                pending,
                quic,
                ..
            } => read_stream_frames(reader, pending, quic.as_ref()).await,
            Self::Ws {
                reader,
                writer,
                deferred_error,
            } => {
                if let Some(error) = deferred_error.take() {
                    return Err(error);
                }
                read_ws_frames(reader, writer, deferred_error).await
            }
        }
    }

    /// The frames available without waiting, possibly none. A clean close
    /// seen here reads as none; the next [`Self::read_frames`] reports it.
    pub(crate) fn try_read_frames(&mut self) -> Result<Vec<Bytes>, LinkError> {
        match self {
            Self::Stream {
                reader,
                pending,
                quic,
                ..
            } => try_read_stream_frames(reader, pending, quic.as_ref()),
            Self::Ws {
                reader,
                deferred_error,
                ..
            } => {
                if let Some(error) = deferred_error.take() {
                    return Err(error);
                }
                try_read_ws_frame(reader)
            }
        }
    }

    pub(crate) async fn write_frame(&mut self, frame: &[u8]) -> Result<(), LinkError> {
        match self {
            Self::Stream { writer, quic, .. } => write_all_within_budget(writer.as_mut(), frame)
                .await
                .map_err(|error| refused_or(error, quic.as_ref())),
            Self::Ws { writer, .. } => {
                budgeted(
                    message_budget(frame.len()),
                    writer.send(frame),
                    LinkError::Ws,
                )
                .await
            }
        }
    }

    /// Write a batch. A byte stream is one `write_all` of the same bytes in
    /// order (one syscall). WebSocket stays one message per frame.
    pub(crate) async fn write_frames(&mut self, frames: &[Vec<u8>]) -> Result<(), LinkError> {
        if let Self::Stream { writer, quic, .. } = self {
            return write_stream_batch(writer.as_mut(), frames)
                .await
                .map_err(|error| refused_or(error, quic.as_ref()));
        }
        for frame in frames {
            self.write_frame(frame).await?;
        }
        Ok(())
    }

    /// A liveness probe: a WebSocket ping, or a protocol `PING` the server
    /// answers with `PONG` on the byte-stream lanes.
    pub(crate) async fn probe(&mut self) -> Result<(), LinkError> {
        match self {
            Self::Stream { .. } => {
                self.write_frame(&encode(&FrameKind::Ping { nonce: 1 }))
                    .await
            }
            Self::Ws { writer, .. } => {
                budgeted(SEND_BUDGET, writer.send_ping(), LinkError::Ws).await
            }
        }
    }

    /// Move a QUIC connection onto a fresh UDP socket of the same family:
    /// QUIC connection migration, which keeps the incarnation and every
    /// stream. `Ok(false)` on the other lanes, which have no such move.
    pub(crate) fn rebind(&mut self, name: &str) -> Result<bool, String> {
        let Self::Stream {
            quic: Some(link), ..
        } = self
        else {
            return Ok(false);
        };
        let local = link
            .endpoint
            .local_addr()
            .map_err(|error| format!("{name}: rebind: no local address: {error}"))?;
        let socket = std::net::UdpSocket::bind(unspecified_like(local))
            .map_err(|error| format!("{name}: rebind: bind a fresh socket: {error}"))?;
        link.endpoint
            .rebind(socket)
            .map_err(|error| format!("{name}: rebind: {error}"))?;
        Ok(true)
    }

    /// Close at once: a QUIC connection and its endpoint are told to go.
    pub(crate) fn close(self) {
        if let Self::Stream {
            quic: Some(link), ..
        } = &self
        {
            link.close();
            link.endpoint.close(quinn::VarInt::from_u32(0), b"");
        }
    }

    /// Close and wait until the QUIC endpoint has delivered the close, so
    /// the server reaps this consumer now. Elsewhere dropping the halves
    /// already is a clean close.
    pub(crate) async fn shutdown(self) {
        if let Self::Stream {
            quic: Some(link), ..
        } = &self
        {
            link.close();
            link.endpoint.wait_idle().await;
        }
    }
}

async fn write_stream_batch(
    writer: &mut (dyn AsyncWrite + Unpin + Send),
    frames: &[Vec<u8>],
) -> Result<(), LinkError> {
    match frames {
        [] => Ok(()),
        [frame] => write_all_within_budget(writer, frame).await,
        many => write_all_within_budget(writer, &join_frames(many)).await,
    }
}

/// Write `bytes`, restarting [`SEND_BUDGET`] every [`SEND_STALL_CHUNK`].
async fn write_all_within_budget(
    writer: &mut (dyn AsyncWrite + Unpin + Send),
    bytes: &[u8],
) -> Result<(), LinkError> {
    let mut offset = 0;
    while offset < bytes.len() {
        let end = offset.saturating_add(SEND_STALL_CHUNK).min(bytes.len());
        budgeted(
            SEND_BUDGET,
            writer.write_all(&bytes[offset..end]),
            LinkError::Write,
        )
        .await?;
        offset = end;
    }
    Ok(())
}

/// [`SEND_BUDGET`] per chunk of a WebSocket message, never past the ceiling.
fn message_budget(len: usize) -> Duration {
    let chunks = len.div_ceil(SEND_STALL_CHUNK).max(1);
    let chunks = u32::try_from(chunks).unwrap_or(u32::MAX);
    SEND_BUDGET.saturating_mul(chunks).min(SEND_BUDGET_CEILING)
}

async fn budgeted<T, E>(
    budget: Duration,
    work: impl std::future::Future<Output = Result<T, E>>,
    map_err: impl FnOnce(E) -> LinkError,
) -> Result<T, LinkError> {
    match tokio::time::timeout(budget, work).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(map_err(error)),
        Err(_elapsed) => Err(LinkError::Stalled),
    }
}

/// Several frames, in order, as one buffer.
fn join_frames(frames: &[Vec<u8>]) -> Vec<u8> {
    let mut len = 0;
    for frame in frames {
        len += frame.len();
    }
    let mut out = Vec::with_capacity(len);
    for frame in frames {
        out.extend_from_slice(frame);
    }
    out
}

/// The unspecified address of `addr`'s family, port 0: what a fresh
/// socket that can still reach the same peer binds.
const fn unspecified_like(addr: std::net::SocketAddr) -> std::net::SocketAddr {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    if addr.is_ipv6() {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
    }
}

async fn read_stream_frames(
    reader: &mut Box<dyn AsyncRead + Unpin + Send + Sync>,
    pending: &mut BytesMut,
    quic: Option<&QuicLink>,
) -> Result<Option<Vec<Bytes>>, LinkError> {
    loop {
        let frames = take_complete_frames(pending)?;
        if !frames.is_empty() {
            return Ok(Some(frames));
        }
        let read = reader
            .read_buf(pending)
            .await
            .map_err(|error| read_error(error, quic))?;
        if read != 0 {
            continue;
        }
        if let Some(reason) = quic.and_then(|link| link.connection.close_reason()) {
            return Err(LinkError::Closed(phux_dial::quic::close_error(&reason)));
        }
        return Ok(None);
    }
}

/// Complete frames already buffered, else whatever one read polled once
/// (against a no-op waker) adds. Sound because these reads are cancel-safe
/// and the next `read_frames().await` re-polls with a real waker.
fn try_read_stream_frames(
    reader: &mut Box<dyn AsyncRead + Unpin + Send + Sync>,
    pending: &mut BytesMut,
    quic: Option<&QuicLink>,
) -> Result<Vec<Bytes>, LinkError> {
    let frames = take_complete_frames(pending)?;
    if !frames.is_empty() {
        return Ok(frames);
    }
    match reader.read_buf(pending).now_or_never() {
        None | Some(Ok(0)) => Ok(Vec::new()),
        Some(Ok(_)) => take_complete_frames(pending),
        Some(Err(error)) => Err(read_error(error, quic)),
    }
}

fn read_error(error: std::io::Error, quic: Option<&QuicLink>) -> LinkError {
    refused_or(LinkError::Read(error), quic)
}

/// A failed read or write. A QUIC stream sees a connection close as a bare
/// "connection lost" in either direction; when that close was the server
/// refusing the bearer token, report the refusal instead, which ends the
/// session (the driver does not redial it) and words it as "re-pair".
fn refused_or(error: LinkError, quic: Option<&QuicLink>) -> LinkError {
    if !matches!(error, LinkError::Read(_) | LinkError::Write(_)) {
        return error;
    }
    match quic
        .and_then(|link| link.connection.close_reason())
        .map(|reason| phux_dial::quic::close_error(&reason))
    {
        Some(refused @ DialError::AuthRefused(_)) => LinkError::Closed(refused),
        _ => error,
    }
}

/// Split a read's complete frames off `pending` without copying them: each
/// frame is a frozen view of the read buffer. A queued frame therefore keeps
/// its read slab alive until the consumer decodes it, bounded by the inbound
/// queue's ceilings (a slab is `pending`'s 64 KiB, or one oversized frame).
fn take_complete_frames(pending: &mut BytesMut) -> Result<Vec<Bytes>, LinkError> {
    let mut frames = Vec::new();
    while frames.len() < MAX_INBOUND_BATCH {
        match framing::split_frame(pending) {
            Ok(Some(frame)) => frames.push(frame.freeze()),
            Err(error) if frames.is_empty() => return Err(LinkError::Framing(error)),
            // Preserve already-decoded frames. On error, the malformed bytes
            // remain at the front of `pending` and fail the next pump step.
            Ok(None) | Err(_) => break,
        }
    }
    Ok(frames)
}

async fn read_ws_frames(
    reader: &mut WsReader,
    writer: &mut WsWriter,
    deferred_error: &mut Option<LinkError>,
) -> Result<Option<Vec<Bytes>>, LinkError> {
    match recv_activity_alive(reader, writer)
        .await
        .map_err(LinkError::Ws)?
    {
        WsActivity::Message(frame) => {
            let mut frames = vec![ws_frame(frame)?];
            while frames.len() < MAX_INBOUND_BATCH {
                match try_read_ws_frame(reader) {
                    Ok(mut more) if !more.is_empty() => frames.append(&mut more),
                    Ok(_) => break,
                    Err(error) => {
                        *deferred_error = Some(error);
                        break;
                    }
                }
            }
            Ok(Some(frames))
        }
        // A pong or a peer ping: no phux payload, but proof of life.
        WsActivity::Control => Ok(Some(vec![Bytes::new()])),
        WsActivity::Closed => Ok(None),
    }
}

/// A message tungstenite already decoded, if any. A close seen here is
/// none; the next read reports it.
fn try_read_ws_frame(reader: &mut WsReader) -> Result<Vec<Bytes>, LinkError> {
    match reader.try_recv_message().map_err(LinkError::Ws)? {
        Some(frame) => Ok(vec![ws_frame(frame)?]),
        None => Ok(Vec::new()),
    }
}

/// One binary message as one frame: the check the server applies to its
/// own inbound messages, since a message that disagrees with its declared
/// length would desynchronize the framed stream.
fn ws_frame(message: Vec<u8>) -> Result<Bytes, LinkError> {
    framing::check_frame(&message).map_err(LinkError::Framing)?;
    Ok(Bytes::from(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rebind_socket_keeps_the_peer_family() {
        let v4 = unspecified_like("127.0.0.1:4433".parse().expect("addr"));
        let v6 = unspecified_like("[::1]:4433".parse().expect("addr"));
        assert!(v4.is_ipv4() && v4.port() == 0);
        assert!(v6.is_ipv6() && v6.port() == 0);
    }

    #[test]
    fn complete_frames_are_delivered_before_a_later_malformed_frame() {
        let mut pending = BytesMut::new();
        FrameKind::Ping { nonce: 7 }.encode(&mut pending);
        pending.extend_from_slice(&[0, 0, 0, 0]);

        let valid = take_complete_frames(&mut pending).expect("valid prefix");

        assert_eq!(valid.len(), 1);
        assert!(take_complete_frames(&mut pending).is_err());
    }

    #[test]
    fn a_stream_read_drains_complete_frames_in_bounded_batches() {
        let mut pending = BytesMut::new();
        for nonce in 0..300 {
            FrameKind::Ping { nonce }.encode(&mut pending);
        }

        let first = take_complete_frames(&mut pending).expect("first batch");
        let second = take_complete_frames(&mut pending).expect("second batch");

        assert_eq!(first.len(), MAX_INBOUND_BATCH);
        assert_eq!(second.len(), 300 - MAX_INBOUND_BATCH);
        assert!(pending.is_empty());
        let (first_frame, tail) = FrameKind::decode(&first[0]).expect("frame decodes");
        assert!(tail.is_empty());
        assert!(matches!(first_frame, FrameKind::Ping { nonce: 0 }));
        let (last_frame, tail) =
            FrameKind::decode(second.last().expect("last frame")).expect("frame decodes");
        assert!(tail.is_empty());
        assert!(matches!(last_frame, FrameKind::Ping { nonce: 299 }));
    }

    /// A non-blocking read takes what is buffered and never waits; a
    /// clean close reads as nothing until the blocking read reports it.
    #[tokio::test]
    async fn a_non_blocking_read_takes_what_is_there_and_never_waits() {
        let (near, mut far) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(near);
        let mut io = Io::stream(Box::new(reader), Box::new(writer));
        assert!(io.try_read_frames().expect("nothing yet").is_empty());
        far.write_all(&encode(&FrameKind::Ping { nonce: 4 }))
            .await
            .expect("write");
        let frames = io.try_read_frames().expect("one frame");
        assert_eq!(frames.len(), 1);
        drop(far);
        assert!(io.try_read_frames().expect("closed").is_empty());
        assert!(io.read_frames().await.expect("clean close").is_none());
    }

    #[test]
    fn joined_frames_keep_order() {
        assert_eq!(join_frames(&[]), Vec::<u8>::new());
        assert_eq!(join_frames(&[vec![1, 2], vec![], vec![3]]), vec![1, 2, 3]);
    }

    #[test]
    fn a_websocket_message_budget_grows_with_its_size_and_then_caps() {
        assert_eq!(message_budget(0), SEND_BUDGET);
        assert_eq!(message_budget(SEND_STALL_CHUNK), SEND_BUDGET);
        assert_eq!(
            message_budget(SEND_STALL_CHUNK + 1),
            SEND_BUDGET.saturating_mul(2)
        );
        assert_eq!(message_budget(16 * 1024 * 1024), SEND_BUDGET_CEILING);
    }

    /// Three chunks, each accepted just inside the budget. The whole write
    /// takes longer than one budget, which is what a slow live link does.
    #[tokio::test(start_paused = true)]
    async fn a_link_that_keeps_taking_chunks_survives_past_one_budget() {
        use futures_util::FutureExt;

        let chunk = SEND_STALL_CHUNK;
        let (mut client, mut peer) = tokio::io::duplex(chunk);
        let payload = vec![7u8; chunk * 3];
        let writing = write_all_within_budget(&mut client, &payload);
        tokio::pin!(writing);

        assert!(
            writing.as_mut().now_or_never().is_none(),
            "the second chunk waits on a full buffer"
        );
        tokio::time::advance(almost_one_budget()).await;
        assert!(
            writing.as_mut().now_or_never().is_none(),
            "the first wait stays inside one budget"
        );
        read_one_chunk(&mut peer, chunk).await;

        assert!(
            writing.as_mut().now_or_never().is_none(),
            "the third chunk waits"
        );
        tokio::time::advance(almost_one_budget()).await;
        assert!(
            writing.as_mut().now_or_never().is_none(),
            "a fresh chunk restarts the budget"
        );
        read_one_chunk(&mut peer, chunk).await;

        let end = writing
            .as_mut()
            .now_or_never()
            .expect("the last chunk fits once the peer reads");
        assert!(end.is_ok(), "{end:?}");
    }

    fn almost_one_budget() -> Duration {
        SEND_BUDGET
            .checked_sub(Duration::from_secs(1))
            .expect("the send budget is longer than one second")
    }

    async fn read_one_chunk(peer: &mut tokio::io::DuplexStream, n: usize) {
        let mut buf = vec![0u8; n];
        peer.read_exact(&mut buf)
            .await
            .expect("the chunk is buffered");
    }
}
