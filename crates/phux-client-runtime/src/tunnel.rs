//! The relay tunnel: how a socket-owning embedder reaches a registered
//! remote phux server the way `phux attach --remote HOST` does (ADR-0007,
//! ADR-0031, ADR-0093 rung 1).
//!
//! The session kernel behind such an embedder is sans-IO: the embedder owns
//! the socket and moves SPEC §5 frames. A local server is a Unix-domain
//! socket the embedder can open itself. A remote one is QUIC or TLS
//! WebSocket with a pinned certificate and a bearer token, which no embedder
//! should reimplement. So the tunnel keeps the embedder's socket model and
//! supplies the far side of it: the embedder creates a connected Unix-domain
//! socket pair, keeps one end for its ordinary framed I/O, and hands the
//! other to a [`Tunnel`], which dials the host on its own thread and relays
//! frames through it byte-for-byte.
//!
//! Framing is untouched in both directions. QUIC carries the same
//! length-prefixed byte stream as a Unix socket, so that lane is a byte copy.
//! WebSocket carries exactly one frame per binary message, so that lane cuts
//! the embedder's byte stream at frame boundaries on the way out and checks
//! each message is exactly one frame on the way in. Nothing is decoded, and
//! the session kernel on the embedder's side never learns which lane it is
//! on.
//!
//! Both lanes run their two directions concurrently. An embedder's socket
//! worker typically reads only between its own writes (Cockpit's does), so a
//! lane that stopped reading the embedder while it delivered to it would
//! deadlock a large paste against heavy output as soon as both socket
//! buffers filled.
//!
//! The bearer token is read from the entry's token file inside the tunnel,
//! just before the dial, and never leaves this thread.

use std::os::unix::net::UnixStream;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;

use futures_util::StreamExt;
use futures_util::stream::SplitStream;
use phux_dial::TlsClientIdentity;
use phux_dial::ws::{Ws, WsKeepalive, WsLiveness, WsWriter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{ReadHalf, WriteHalf};
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use crate::dial::{
    DIAL_TIMEOUT, check_inbound, dial_message, load_token, plan_quic, plan_ws, quic_closed_message,
    quic_stream_lost, stalled, take_frame, timed_out,
};
use crate::target::{Resolved, Transport};

#[cfg(test)]
const TEST_PANIC_HOST: &str = "__phux_test_tunnel_panic__";

/// Where a tunnel is in its life. The two terminal states never change.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TunnelState {
    /// Registry entry found; no network activity yet.
    Resolved = 0,
    /// The transport socket was accepted; dialing.
    Connecting = 1,
    /// Transport established; frames are being relayed.
    Connected = 2,
    /// Resolution or the connection failed; the message says why. Terminal.
    Failed = 3,
    /// The embedder closed its end, or the tunnel was dropped. Terminal.
    Closed = 4,
}

impl TunnelState {
    /// The stable numeric form a C ABI carries.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    const fn from_u32(value: u32) -> Self {
        match value {
            0 => Self::Resolved,
            1 => Self::Connecting,
            2 => Self::Connected,
            3 => Self::Failed,
            _ => Self::Closed,
        }
    }

    /// Whether no further transition can happen.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Failed | Self::Closed)
    }
}

/// State shared with the tunnel thread.
///
/// The message is written once, before the FAILED state is published, so a
/// reader that observes FAILED always finds a stable message it may borrow
/// for the tunnel's lifetime.
#[derive(Debug)]
pub struct TunnelShared {
    state: AtomicU32,
    message: OnceLock<String>,
}

impl TunnelShared {
    const fn with_state(state: TunnelState) -> Self {
        Self {
            state: AtomicU32::new(state.as_u32()),
            message: OnceLock::new(),
        }
    }

    /// The current state.
    #[must_use]
    pub fn state(&self) -> TunnelState {
        TunnelState::from_u32(self.state.load(Ordering::Acquire))
    }

    /// Why the tunnel failed. `None` unless [`TunnelState::Failed`] is
    /// observable, so a reader never sees a message before its state.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        if self.state() == TunnelState::Failed {
            self.message.get().map(String::as_str)
        } else {
            None
        }
    }

    fn try_start(&self) -> bool {
        self.state
            .compare_exchange(
                TunnelState::Resolved.as_u32(),
                TunnelState::Connecting.as_u32(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn connected(&self) {
        let _ = self.state.compare_exchange(
            TunnelState::Connecting.as_u32(),
            TunnelState::Connected.as_u32(),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// Publish the reason and the FAILED state, in that order. The one warn
    /// event for a tunnel failure is emitted here, so every caller's reason
    /// reaches the log exactly once.
    fn fail(&self, message: String) {
        if self.state().is_terminal() {
            return;
        }
        tracing::warn!(reason = %message, "remote tunnel failed");
        let _ = self.message.set(message);
        self.state
            .store(TunnelState::Failed.as_u32(), Ordering::Release);
    }

    fn close(&self) {
        if !self.state().is_terminal() {
            tracing::info!("remote tunnel closed");
            self.state
                .store(TunnelState::Closed.as_u32(), Ordering::Release);
        }
    }
}

/// Why [`Tunnel::start`] refused.
#[derive(Debug)]
pub enum TunnelStartError {
    /// The tunnel is not in the RESOLVED state: it was already started.
    NotResolved,
    /// The relay thread could not be spawned; the tunnel is now FAILED.
    Spawn(std::io::Error),
}

impl std::fmt::Display for TunnelStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotResolved => formatter.write_str("remote tunnel is not in the RESOLVED state"),
            Self::Spawn(err) => write!(formatter, "could not start the tunnel thread: {err}"),
        }
    }
}

impl std::error::Error for TunnelStartError {}

/// One resolved host and at most one relay thread.
///
/// Dropping the tunnel cancels any dial or relay and joins the thread,
/// bounded by one scheduler poll rather than by the network. The state is
/// readable from any thread while the tunnel lives, including concurrently
/// with [`Tunnel::start`], because nothing `start` changes is reachable
/// except through atomics and the thread mutex.
#[derive(Debug)]
pub struct Tunnel {
    resolved: Resolved,
    shared: Arc<TunnelShared>,
    cancel: Arc<Notify>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Tunnel {
    /// A RESOLVED tunnel for `resolved`. Nothing is read or dialed.
    #[must_use]
    pub fn new(resolved: Resolved) -> Self {
        Self {
            resolved,
            shared: Arc::new(TunnelShared::with_state(TunnelState::Resolved)),
            cancel: Arc::new(Notify::new()),
            thread: Mutex::new(None),
        }
    }

    /// The registry entry this tunnel dials. Holds no secret.
    #[must_use]
    pub const fn resolved(&self) -> &Resolved {
        &self.resolved
    }

    /// The transport lane the entry's endpoint selects.
    #[must_use]
    pub const fn transport(&self) -> &Transport {
        &self.resolved.transport
    }

    /// Publish a failure as a test would observe one, without a network.
    #[cfg(any(test, feature = "testing"))]
    pub fn inject_failure(&self, message: String) {
        self.shared.fail(message);
    }

    /// The current state.
    #[must_use]
    pub fn state(&self) -> TunnelState {
        self.shared.state()
    }

    /// Why the tunnel failed, once it has.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.shared.message()
    }

    /// Start dialing, relaying frames through `stream`: one end of a
    /// connected `SOCK_STREAM` Unix-domain socket pair whose other end the
    /// embedder keeps for its ordinary framed I/O.
    ///
    /// When the dial or connection fails the tunnel publishes FAILED and its
    /// message, then drops the stream, so the embedder reads EOF only after
    /// the reason is readable. On macOS the embedder should set
    /// `SO_NOSIGPIPE` on both ends first: the tunnel writes from a library
    /// thread and cannot change the process's `SIGPIPE` disposition.
    ///
    /// `&self`: the one-shot RESOLVED -> CONNECTING step is an atomic
    /// compare-exchange and the join handle sits behind a mutex, so a
    /// concurrent state read never aliases a unique borrow.
    pub fn start(&self, stream: UnixStream) -> Result<(), TunnelStartError> {
        if !self.shared.try_start() {
            return Err(TunnelStartError::NotResolved);
        }
        let resolved = self.resolved.clone();
        tracing::info!(host = %resolved.name, "remote tunnel starting");
        let shared = Arc::clone(&self.shared);
        let cancel = Arc::clone(&self.cancel);
        let spawned = std::thread::Builder::new()
            .name("phux-remote-tunnel".to_owned())
            .spawn(move || run(&shared, &cancel, &resolved, stream));
        match spawned {
            Ok(thread) => {
                *self.thread.lock().unwrap_or_else(PoisonError::into_inner) = Some(thread);
                Ok(())
            }
            Err(err) => {
                self.shared
                    .fail(format!("could not start the tunnel thread: {err}"));
                Err(TunnelStartError::Spawn(err))
            }
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        // The pump selects on this notification at every await, so the join
        // below is bounded by one poll, not by a dial or a relay.
        self.cancel.notify_one();
        let slot = self
            .thread
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(thread) = slot.take() {
            let _ = thread.join();
        }
    }
}

/// Run one tunnel to completion on the calling (dedicated) thread.
///
/// The terminal state is published BEFORE the embedder's socket is dropped,
/// so an embedder that reads EOF and then asks why always finds the answer.
///
/// A Rust panic on this thread used to abort the host process (Cockpit
/// links the FFI with `panic = unwind`, and this is the one remote-dial
/// thread the C ABI's `catch_unwind` does not wrap). Contain it, publish
/// FAILED, then drop the socket so the embedder still reads a reason
/// before EOF.
///
/// This does not contain SIGILL. Cockpit 0.23.3 aborted on Apple ARM64
/// during QUIC TLS 1.3 signature verify (`p256_mul_mont`) because
/// `dead_strip` dropped ring's local helpers. That is
/// `keepRingP256Helpers` in the Cockpit build, not a trust-policy bug:
/// [`phux_dial::CertTrust::Pinned`] and [`phux_dial::CertTrust::SkipVerify`]
/// both still call ring ECDSA.
fn run(shared: &Arc<TunnelShared>, cancel: &Arc<Notify>, resolved: &Resolved, stream: UnixStream) {
    // Keep the embedder's pair open across an unwind so FAILED is published
    // before EOF, matching the ordinary failure contract.
    let hold = stream.try_clone().ok();
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        run_inner(shared, cancel, resolved, stream);
    }));
    if panicked.is_err() {
        shared.fail("the remote tunnel aborted unexpectedly".to_owned());
    }
    drop(hold);
}

fn run_inner(
    shared: &Arc<TunnelShared>,
    cancel: &Arc<Notify>,
    resolved: &Resolved,
    stream: UnixStream,
) {
    #[cfg(test)]
    assert!(resolved.name != TEST_PANIC_HOST, "injected tunnel panic");
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            shared.fail(format!("could not start the tunnel runtime: {err}"));
            drop(stream);
            return;
        }
    };
    runtime.block_on(async {
        let Some(mut socket) = adopt(shared, stream) else {
            return;
        };
        let outcome = tokio::select! {
            () = cancel.notified() => {
                tracing::info!(host = %resolved.name, "remote tunnel cancelled by the embedder");
                Ok(())
            }
            outcome = serve(shared, resolved, &mut socket) => outcome,
        };
        match outcome {
            Ok(()) => shared.close(),
            Err(reason) => shared.fail(reason),
        }
        drop(socket);
    });
    // A name lookup still parked on the blocking pool must not hold the
    // embedder's drop hostage; the process owns no result it could deliver.
    runtime.shutdown_background();
}

/// Hand the embedder's socket to tokio. Every failure is published while a
/// descriptor still holds the socket open, so the embedder never reads EOF
/// before it can read why.
fn adopt(shared: &TunnelShared, stream: UnixStream) -> Option<tokio::net::UnixStream> {
    let unusable =
        |err: std::io::Error| format!("the embedder's transport socket is unusable: {err}");
    if let Err(err) = stream.set_nonblocking(true) {
        shared.fail(unusable(err));
        return None;
    }
    // `from_std` consumes the stream even when it fails; this duplicate is
    // what keeps the socket open until the reason is published.
    let hold = match stream.try_clone() {
        Ok(hold) => hold,
        Err(err) => {
            shared.fail(unusable(err));
            return None;
        }
    };
    match tokio::net::UnixStream::from_std(stream) {
        Ok(socket) => Some(socket),
        Err(err) => {
            shared.fail(unusable(err));
            drop(hold);
            None
        }
    }
}

/// `Ok` means the embedder closed its end; every other ending is a reason.
async fn serve(
    shared: &TunnelShared,
    resolved: &Resolved,
    socket: &mut tokio::net::UnixStream,
) -> Result<(), String> {
    match &resolved.transport {
        Transport::Quic(authority) => serve_quic(shared, resolved, authority, socket).await,
        Transport::Ws(url) => serve_ws(shared, resolved, url, socket).await,
    }
}

async fn serve_quic(
    shared: &TunnelShared,
    resolved: &Resolved,
    authority: &str,
    socket: &mut tokio::net::UnixStream,
) -> Result<(), String> {
    let name = resolved.name.as_str();
    let started = std::time::Instant::now();
    let established = tokio::time::timeout(DIAL_TIMEOUT, async {
        let dial = plan_quic(resolved, authority).await?;
        tracing::info!(host = name, transport = "quic", addr = %dial.addr, "remote tunnel dialing");
        // Embedders must not inherit `PHUX_WORKLOAD_*` from a launcher shell.
        let connected = phux_dial::quic::dial_with_identity(&dial, &TlsClientIdentity::None)
            .await
            .map_err(|err| dial_message(name, &err));
        // The bearer preamble has been written; the owned token goes now,
        // not at the end of the session.
        drop(dial);
        connected
    })
    .await
    .map_err(|_| timed_out(name))??;
    let (endpoint, connection, mut to_host, mut from_host) = established;
    if let Some(err) = connection.close_reason() {
        return Err(quic_closed_message(name, &err));
    }
    tracing::info!(
        host = name,
        transport = "quic",
        elapsed_ms = started.elapsed().as_millis(),
        "remote tunnel connected; relaying frames"
    );
    shared.connected();
    let (mut from_embedder, mut to_embedder) = socket.split();
    let result = tokio::select! {
        biased;
        err = connection.closed() => Err(quic_closed_message(name, &err)),
        outbound = tokio::io::copy(&mut from_embedder, &mut to_host) => outbound
            .map(|sent| tracing::info!(host = name, bytes_sent = sent, "embedder closed its end"))
            .map_err(|err| quic_stream_lost(name, &connection, err)),
        inbound = tokio::io::copy(&mut from_host, &mut to_embedder) => Err(match inbound {
            Ok(_) => connection.close_reason().map_or_else(
                || format!("{name} closed the connection"),
                |close| quic_closed_message(name, &close),
            ),
            Err(err) => quic_stream_lost(name, &connection, err),
        }),
    };
    connection.close(quinn::VarInt::from_u32(0), b"tunnel closed");
    endpoint.close(quinn::VarInt::from_u32(0), b"");
    result
}

async fn serve_ws(
    shared: &TunnelShared,
    resolved: &Resolved,
    url: &str,
    socket: &mut tokio::net::UnixStream,
) -> Result<(), String> {
    let name = resolved.name.as_str();
    let started = std::time::Instant::now();
    let ws = tokio::time::timeout(DIAL_TIMEOUT, async {
        let dial = plan_ws(resolved, url, load_token(resolved)?)?;
        tracing::info!(host = name, transport = "ws", url, "remote tunnel dialing");
        // Embedders must not inherit `PHUX_WORKLOAD_*` from a launcher shell.
        let connected = phux_dial::ws::dial_with_identity(&dial, &TlsClientIdentity::None)
            .await
            .map_err(|err| dial_message(name, &err));
        // The Authorization header has been sent; drop the owned token now.
        drop(dial);
        connected
    })
    .await
    .map_err(|_| timed_out(name))??;
    tracing::info!(
        host = name,
        transport = "ws",
        elapsed_ms = started.elapsed().as_millis(),
        "remote tunnel connected; relaying frames"
    );
    shared.connected();
    relay_ws(name, ws, socket).await
}

/// The WebSocket lane as two concurrent halves, like the QUIC lane's two
/// copies. Keepalive pings are asked for by the inbound half, which owns the
/// liveness clock, and sent by the outbound half, which owns the writer.
async fn relay_ws(name: &str, ws: Ws, socket: &mut tokio::net::UnixStream) -> Result<(), String> {
    let (tx, rx) = ws.split();
    let (pings_tx, pings_rx) = mpsc::channel(1);
    let (mut from_embedder, mut to_embedder) = socket.split();
    tokio::select! {
        outbound = embedder_to_host(name, &mut from_embedder, WsWriter { tx }, pings_rx) => outbound,
        inbound = host_to_embedder(name, rx, &mut to_embedder, pings_tx) => inbound,
    }
}

/// `Ok` only when the embedder closed its end.
async fn embedder_to_host(
    name: &str,
    from_embedder: &mut ReadHalf<'_>,
    mut writer: WsWriter,
    mut pings: mpsc::Receiver<()>,
) -> Result<(), String> {
    let mut pending = bytes::BytesMut::with_capacity(64 * 1024);
    loop {
        // Both arms are cancel-safe: `read_buf` keeps partial bytes in
        // `pending`, and a ping request is a unit with no payload to lose.
        tokio::select! {
            read = from_embedder.read_buf(&mut pending) => match read {
                Ok(0) => {
                    tracing::info!(host = name, "embedder closed its end");
                    return Ok(());
                }
                Ok(_) => forward_frames(name, &mut pending, &mut writer).await?,
                Err(err) => return Err(format!("{name}: reading the embedder's frames failed: {err}")),
            },
            Some(()) = pings.recv() => writer
                .send_ping()
                .await
                .map_err(|err| dial_message(name, &err))?,
        }
    }
}

/// Never `Ok`: the host ending the stream is always a reason.
async fn host_to_embedder(
    name: &str,
    mut rx: SplitStream<Ws>,
    to_embedder: &mut WriteHalf<'_>,
    pings: mpsc::Sender<()>,
) -> Result<(), String> {
    // The same liveness policy `recv_message_alive` applies, kept here
    // because the writer that sends the ping lives in the other half.
    let mut keepalive = WsKeepalive::new(Instant::now());
    loop {
        let nap = match keepalive.poll(Instant::now()) {
            WsLiveness::Dead => return Err(stalled(name)),
            WsLiveness::Ping => {
                keepalive.note_ping(Instant::now());
                // Capacity one: a ping already queued answers this request.
                let _ = pings.try_send(());
                continue;
            }
            WsLiveness::Idle(nap) => nap,
        };
        let Ok(next) = tokio::time::timeout(nap, rx.next()).await else {
            continue;
        };
        match next {
            None | Some(Ok(Message::Close(_))) => {
                return Err(format!("{name} closed the connection"));
            }
            Some(Err(err)) => return Err(format!("{name}: the connection was lost: {err}")),
            Some(Ok(Message::Binary(frame))) => {
                keepalive.note_inbound(Instant::now());
                check_inbound(name, &frame)?;
                to_embedder
                    .write_all(&frame)
                    .await
                    .map_err(|err| format!("{name}: delivering a frame failed: {err}"))?;
                // Time spent waiting on the embedder is not the host's
                // silence.
                keepalive.note_inbound(Instant::now());
            }
            // Text, ping, pong: not a phux frame, but proof of life.
            Some(Ok(_)) => keepalive.note_inbound(Instant::now()),
        }
    }
}

/// Send every complete frame buffered in `pending` as one binary message,
/// leaving a partial tail for the next read.
async fn forward_frames(
    name: &str,
    pending: &mut bytes::BytesMut,
    writer: &mut WsWriter,
) -> Result<(), String> {
    while let Some(frame) = take_frame(pending).map_err(|err| format!("{name}: {err}"))? {
        writer
            .send(&frame)
            .await
            .map_err(|err| dial_message(name, &err))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_panic_on_the_tunnel_thread_publishes_failed_before_eof() {
        let (mut embedder, tunnel_end) = UnixStream::pair().expect("pair");
        embedder
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("timeout");
        let shared = Arc::new(TunnelShared::with_state(TunnelState::Connecting));
        let cancel = Arc::new(Notify::new());
        let target = Resolved {
            name: TEST_PANIC_HOST.to_owned(),
            endpoint: "ws://127.0.0.1:1".to_owned(),
            session: None,
            transport: Transport::Ws("ws://127.0.0.1:1".to_owned()),
            token_file: Some(PathBuf::from("/secret/mini.token")),
            cert_fingerprint: None,
        };
        let joined = std::thread::spawn({
            let shared = Arc::clone(&shared);
            move || run(&shared, &cancel, &target, tunnel_end)
        })
        .join();
        assert!(
            joined.is_ok(),
            "a panic on the tunnel thread must not unwind into the host process"
        );
        assert_eq!(shared.state(), TunnelState::Failed);
        assert_eq!(
            shared.message(),
            Some("the remote tunnel aborted unexpectedly")
        );
        let mut byte = [0u8; 1];
        assert_eq!(embedder.read(&mut byte).expect("eof"), 0);
    }

    #[test]
    fn a_tunnel_starts_once_and_reads_its_own_state() {
        let tunnel = Tunnel::new(Resolved {
            name: "mini".to_owned(),
            endpoint: "ws://127.0.0.1:1".to_owned(),
            session: Some("work".to_owned()),
            transport: Transport::Ws("ws://127.0.0.1:1".to_owned()),
            token_file: None,
            cert_fingerprint: None,
        });
        assert_eq!(tunnel.state(), TunnelState::Resolved);
        assert!(tunnel.message().is_none());
        assert!(matches!(tunnel.transport(), Transport::Ws(_)));
        let (_embedder, tunnel_end) = UnixStream::pair().expect("pair");
        tunnel.start(tunnel_end).expect("first start");
        let (_embedder, again) = UnixStream::pair().expect("pair");
        assert!(matches!(
            tunnel.start(again),
            Err(TunnelStartError::NotResolved)
        ));
        // Dropping joins the thread: a refused loopback port fails fast, and
        // cancellation bounds the rest.
        drop(tunnel);
    }
}
