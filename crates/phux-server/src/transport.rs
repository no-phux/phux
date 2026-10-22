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

mod uds;
mod ws;
mod ws_auth;
mod ws_upgrade;

pub(crate) use self::uds::*;
pub(crate) use self::ws::*;
pub(crate) use self::ws_auth::*;
use self::ws_upgrade::{WsAcceptStage, WsPeerRejection, ws_accept_error, ws_admit};
#[cfg(test)]
use self::ws_upgrade::{classify_ws_upgrade_error, classify_ws_upgrade_stage};

use std::io;
use std::net::IpAddr;
use std::time::Duration;

use bytes::BytesMut;
use futures_util::StreamExt;
use futures_util::future::LocalBoxFuture;
use futures_util::stream::FuturesUnordered;
use phux_protocol::policy::TransportType;

use phux_protocol::wire::framing::LENGTH_PREFIX_LEN as LENGTH_PREFIX;
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
    /// message-oriented transport requires; byte-stream transports override
    /// it with one write of the whole batch. A writer may take the batch's
    /// bytes rather than copy them (WebSocket does), so callers clear it
    /// before reuse.
    async fn write_frames(&mut self, batch: &mut BytesMut, ends: &[usize]) -> io::Result<()> {
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

#[cfg(test)]
mod tests;
