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
use phux_dial::DialError;
use phux_dial::ws::{WsActivity, WsReader, WsWriter, recv_activity_alive};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::framing::FramingError;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

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

mod child;
mod read;

use child::ChildLink;

use read::{
    read_stream_frames, read_ws_frames, refused_or, try_read_stream_frames, try_read_ws_frame,
};

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
    /// stream, not an end-to-end session a relay splices (docs/protocol.md#quic-stream-routing,
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
        /// Own the stdio bridge and its stderr drainer for this link's life.
        child: Option<ChildLink>,
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
    #[cfg(unix)]
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
        let (reader, writer) = host_halves(stream);
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
            child: None,
        }
    }

    /// Adopt a spawned stdio bridge. Missing pipes are a failed dial.
    #[cfg(any(windows, test))]
    pub(crate) fn child(command: &mut tokio::process::Command) -> std::io::Result<Self> {
        let (child, reader, writer) = ChildLink::spawn(command)?;
        let mut link = Self::framed(Box::new(reader), Box::new(writer), None, None);
        if let Self::Stream { child: owner, .. } = &mut link {
            *owner = Some(child);
        }
        Ok(link)
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
                child,
                ..
            } => {
                let frames = read_stream_frames(reader, pending, quic.as_ref()).await?;
                match (frames, child) {
                    (None, Some(child)) => Err(LinkError::Read(child.closed().await)),
                    (frames, _) => Ok(frames),
                }
            }
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

/// The plain QUIC halves, or the end-to-end TLS session inside the stream.
pub(crate) fn host_halves(
    stream: phux_dial::quic::DialedStream,
) -> (
    Box<dyn AsyncRead + Send + Sync + Unpin>,
    Box<dyn AsyncWrite + Send + Sync + Unpin>,
) {
    match stream {
        phux_dial::quic::DialedStream::Plain { send, recv } => (Box::new(recv), Box::new(send)),
        phux_dial::quic::DialedStream::Inner(stream) => {
            let (recv, send) = tokio::io::split(*stream);
            (Box::new(recv), Box::new(send))
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

#[cfg(test)]
mod tests;
