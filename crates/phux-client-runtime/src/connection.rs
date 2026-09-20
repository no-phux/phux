//! The async connection driver: dial, framing, keepalive, the reconnect
//! ladder, and the pump that feeds the sans-IO control plane.
//!
//! One [`run_session`] future owns a session's whole life: it dials the
//! [`Target`] over its lane, writes the frames the [`ControlPlane`] queues,
//! feeds it every inbound frame, and when the transport drops walks the
//! [`Ladder`] from `crate::reconnect`, with [`is_fatal_refusal`] ending the
//! session terminally instead. A resync (a replica the kernel invalidated,
//! or a consumer's request) redials at once; a nudge cuts a backoff short
//! and probes a possibly-stale socket. The driver runs on whatever tokio
//! runtime polls it; [`crate::runtime`] gives it one background thread so
//! a synchronous binding drives the session with plain method calls.
//!
//! The control plane is shared with those callers through a mutex. The
//! driver never calls foreign code while holding it: the wake callback runs
//! after the lock is released.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bytes::BytesMut;
use futures_util::StreamExt;
use phux_dial::TlsClientIdentity;
use phux_dial::ws::{WsActivity, WsReader, WsWriter, recv_activity_alive};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::framing;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, watch};

use crate::control::{ControlError, ControlPlane, Status, encode};
use crate::dial::{
    DIAL_TIMEOUT, dial_message, load_token, plan_quic, plan_ws, quic_closed_message, timed_out,
};
use crate::reconnect::{Ladder, is_fatal_refusal};
use crate::target::{Resolved, Transport as Lane};

/// The lane a session rides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// The local server's Unix-domain socket.
    Uds(PathBuf),
    /// A `ws://` or `wss://` URL. Off loopback it needs the pin and token
    /// a registry entry carries; see [`Target::resolve`].
    Ws(String),
    /// A `HOST:PORT` QUIC authority. Off loopback it needs the pin and
    /// token a registry entry carries.
    Quic(String),
}

impl Transport {
    /// The lane's name as a log field.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Uds(_) => "uds",
            Self::Ws(_) => "ws",
            Self::Quic(_) => "quic",
        }
    }
}

/// Where a session connects and what it trusts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The lane.
    pub transport: Transport,
    /// The host's name, for wording.
    pub name: String,
    /// The SHA-256 leaf fingerprint to pin, or `None` for loopback.
    pub cert_fingerprint: Option<String>,
    /// Where the bearer token lives; read only at dial time.
    pub token_file: Option<PathBuf>,
}

impl Target {
    /// The local server at `path`.
    #[must_use]
    pub fn uds(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            name: path.display().to_string(),
            transport: Transport::Uds(path),
            cert_fingerprint: None,
            token_file: None,
        }
    }

    /// A loopback WebSocket server, plaintext and unpaired.
    #[must_use]
    pub fn ws(url: impl Into<String>) -> Self {
        let url = url.into();
        Self {
            name: url.clone(),
            transport: Transport::Ws(url),
            cert_fingerprint: None,
            token_file: None,
        }
    }

    /// A loopback QUIC listener, unpaired.
    #[must_use]
    pub fn quic(authority: impl Into<String>) -> Self {
        let authority = authority.into();
        Self {
            name: authority.clone(),
            transport: Transport::Quic(authority),
            cert_fingerprint: None,
            token_file: None,
        }
    }

    /// A registered host: `[USER@]HOST[:PORT]` resolved against the CLI's
    /// `[[remote]]` registry, which supplies the pin and token file.
    pub fn resolve(raw: &str, config_path: Option<&Path>) -> Result<Self, String> {
        crate::target::resolve(raw, config_path).map(Self::from)
    }

    /// The registry entry a WebSocket or QUIC dial is planned from.
    fn resolved(&self) -> Option<Resolved> {
        let (endpoint, transport) = match &self.transport {
            Transport::Uds(_) => return None,
            Transport::Ws(url) => (url.clone(), Lane::Ws(url.clone())),
            Transport::Quic(authority) => {
                (format!("quic://{authority}"), Lane::Quic(authority.clone()))
            }
        };
        Some(Resolved {
            name: self.name.clone(),
            endpoint,
            session: None,
            transport,
            token_file: self.token_file.clone(),
            cert_fingerprint: self.cert_fingerprint.clone(),
        })
    }
}

impl From<Resolved> for Target {
    fn from(resolved: Resolved) -> Self {
        let transport = match &resolved.transport {
            Lane::Quic(authority) => Transport::Quic(authority.clone()),
            Lane::Ws(url) => Transport::Ws(url.clone()),
        };
        Self {
            transport,
            name: resolved.name,
            cert_fingerprint: resolved.cert_fingerprint,
            token_file: resolved.token_file,
        }
    }
}

/// The reconnect policy a session runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectOptions {
    /// The backoff ladder between attempts.
    pub ladder: Ladder,
    /// Attempts a session that never attached gets before it fails.
    pub initial_attempts: u32,
    /// Wall-clock cap on the never-attached phase, whichever comes first
    /// with `initial_attempts`; a consumer waiting for the attach derives
    /// its deadline from this.
    pub initial_budget: Duration,
    /// How long a nudge's liveness probe waits for any inbound traffic.
    pub probe_timeout: Duration,
    /// The bound on one dial.
    pub dial_timeout: Duration,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            ladder: Ladder::INTERACTIVE,
            initial_attempts: 5,
            initial_budget: Duration::from_secs(18),
            probe_timeout: Duration::from_secs(3),
            dial_timeout: DIAL_TIMEOUT,
        }
    }
}

/// The control plane, shared between the driver and the caller threads.
pub type Shared = Arc<Mutex<ControlPlane>>;

/// Lock the shared control plane, surviving a poisoned mutex.
pub fn lock(shared: &Shared) -> MutexGuard<'_, ControlPlane> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The signals a caller raises at the driver.
#[derive(Debug)]
pub struct Signals {
    /// Frames were queued on the control plane.
    pub outbound: Arc<Notify>,
    /// Drop the socket and redial for fresh snapshots.
    pub resync: watch::Receiver<u64>,
    /// Cut a backoff short; probe an attached socket.
    pub nudge: watch::Receiver<u64>,
    /// End the session for good.
    pub close: watch::Receiver<bool>,
}

/// Called after the driver changes consumer-visible state, outside the
/// control-plane lock.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// How one connection ended, deciding the session loop's next move.
#[derive(Debug)]
enum ConnectionEnd {
    /// The consumer closed the session, or the server detached it at the
    /// consumer's request: clean stop.
    Closed,
    /// The server closed the socket or the transport failed.
    Dropped(Option<String>),
    /// A refusal no retry can satisfy.
    Refused(String),
    /// Fresh snapshots were asked for: reconnect at once.
    Resync,
}

enum Decision {
    Stop,
    RetryNow,
    RetryBackoff,
}

/// Run one session to its end: connect, then reconnect with backoff on
/// drops, until the consumer closes it or a refusal ends it.
pub async fn run_session(
    target: Target,
    options: ConnectOptions,
    shared: Shared,
    mut signals: Signals,
    wake: Wake,
) {
    let mut backoff = options.ladder.floor;
    let mut attempts: u32 = 0;
    // The never-attached deadline: armed from the first dial, moot once a
    // handshake lands. It bounds how long a session may spend having never
    // attached, never how long an attached session may run.
    let ladder_deadline = tokio::time::Instant::now() + options.initial_budget;
    loop {
        attempts += 1;
        let Some((end, was_attached)) =
            run_attempt(&target, options, &shared, &mut signals, ladder_deadline).await
        else {
            fail(&shared, None, &wake);
            return;
        };
        // Attached at the moment the connection ended means it was
        // healthy until the drop: the next ladder starts from the floor.
        if was_attached {
            backoff = options.ladder.floor;
        }
        match finish_attempt(&shared, end, attempts, options.initial_attempts) {
            Decision::Stop => {
                wake();
                return;
            }
            Decision::RetryNow => wake(),
            Decision::RetryBackoff => {
                wake();
                tokio::select! {
                    () = tokio::time::sleep(backoff) => {}
                    () = signal(&mut signals.nudge) => {}
                    () = signal(&mut signals.resync) => {}
                    () = closed(&mut signals.close) => {
                        lock(&shared).close();
                        wake();
                        return;
                    }
                }
                backoff = options.ladder.next(backoff);
            }
        }
    }
}

/// One attempt under the never-attached budget. `None` means the budget
/// ran out before any attach.
async fn run_attempt(
    target: &Target,
    options: ConnectOptions,
    shared: &Shared,
    signals: &mut Signals,
    ladder_deadline: tokio::time::Instant,
) -> Option<(ConnectionEnd, bool)> {
    let connection = run_connection(target, options, shared, signals);
    if lock(shared).attached_once() {
        return Some(connection.await);
    }
    tokio::pin!(connection);
    let budget = tokio::time::sleep_until(ladder_deadline);
    tokio::pin!(budget);
    let mut armed = true;
    loop {
        tokio::select! {
            end = &mut connection => return Some(end),
            () = &mut budget, if armed => {
                if lock(shared).attached_once() {
                    armed = false;
                    continue;
                }
                return None;
            }
        }
    }
}

fn fail(shared: &Shared, message: Option<String>, wake: &Wake) {
    let mut control = lock(shared);
    let message = message
        .or_else(|| control.last_error().map(str::to_owned))
        .unwrap_or_else(|| "connect: no answer within the initial budget".to_owned());
    control.fail(message);
    drop(control);
    wake();
}

fn finish_attempt(
    shared: &Shared,
    end: ConnectionEnd,
    attempts: u32,
    initial_attempts: u32,
) -> Decision {
    let mut control = lock(shared);
    match end {
        ConnectionEnd::Closed => {
            control.close();
            Decision::Stop
        }
        ConnectionEnd::Resync => {
            control.connection_lost(None);
            Decision::RetryNow
        }
        ConnectionEnd::Refused(message) => {
            control.fail(message);
            Decision::Stop
        }
        ConnectionEnd::Dropped(message) => {
            if control.attached_once() || attempts < initial_attempts {
                control.connection_lost(message);
                Decision::RetryBackoff
            } else {
                control.fail(message.unwrap_or_else(|| "server closed".to_owned()));
                Decision::Stop
            }
        }
    }
}

/// Wait for a watch signal; a dropped sender parks forever rather than
/// busy-spinning on the closed-channel error.
async fn signal(rx: &mut watch::Receiver<u64>) {
    if rx.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

async fn closed(rx: &mut watch::Receiver<bool>) {
    if *rx.borrow() {
        return;
    }
    if rx.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// One established lane, read and written as SPEC section 5 frames.
enum Io {
    Stream {
        reader: Box<dyn AsyncRead + Unpin + Send>,
        writer: Box<dyn AsyncWrite + Unpin + Send>,
        pending: BytesMut,
        /// Kept so the QUIC endpoint's driver outlives the connection and a
        /// clean close can be issued.
        quic: Option<(quinn::Endpoint, quinn::Connection)>,
    },
    Ws {
        reader: WsReader,
        writer: WsWriter,
    },
}

impl Io {
    /// The next inbound frame, or `Ok(None)` on a clean close. Cancel-safe:
    /// partial bytes stay buffered.
    async fn read_frame(&mut self, name: &str) -> Result<Option<Vec<u8>>, String> {
        match self {
            Self::Stream {
                reader,
                pending,
                quic,
                ..
            } => loop {
                if let Some(frame) =
                    framing::split_frame(pending).map_err(|error| format!("{name}: {error}"))?
                {
                    return Ok(Some(frame.to_vec()));
                }
                let read = reader
                    .read_buf(pending)
                    .await
                    .map_err(|error| format!("{name}: the connection was lost: {error}"))?;
                if read == 0 {
                    if let Some((_, connection)) = quic
                        && let Some(reason) = connection.close_reason()
                    {
                        return Err(quic_closed_message(name, &reason));
                    }
                    return Ok(None);
                }
            },
            Self::Ws { reader, writer } => match recv_activity_alive(reader, writer)
                .await
                .map_err(|error| dial_message(name, &error))?
            {
                WsActivity::Message(frame) => {
                    framing::check_frame(&frame)
                        .map_err(|error| format!("{name} sent a malformed frame: {error}"))?;
                    Ok(Some(frame))
                }
                // A pong or a peer ping: no phux payload, but proof of life.
                WsActivity::Control => Ok(Some(Vec::new())),
                WsActivity::Closed => Ok(None),
            },
        }
    }

    async fn write_frame(&mut self, name: &str, frame: &[u8]) -> Result<(), String> {
        match self {
            Self::Stream { writer, .. } => writer
                .write_all(frame)
                .await
                .map_err(|error| format!("{name}: send failed: {error}")),
            Self::Ws { writer, .. } => writer
                .send(frame)
                .await
                .map_err(|error| dial_message(name, &error)),
        }
    }

    /// A liveness probe: a WebSocket ping, or a protocol `PING` the server
    /// answers with `PONG` on the byte-stream lanes.
    async fn probe(&mut self, name: &str) -> Result<(), String> {
        match self {
            Self::Stream { .. } => {
                self.write_frame(name, &encode(&FrameKind::Ping { nonce: 1 }))
                    .await
            }
            Self::Ws { writer, .. } => writer
                .send_ping()
                .await
                .map_err(|error| dial_message(name, &error)),
        }
    }

    fn close(self) {
        if let Self::Stream {
            quic: Some((endpoint, connection)),
            ..
        } = self
        {
            connection.close(quinn::VarInt::from_u32(0), b"session closed");
            endpoint.close(quinn::VarInt::from_u32(0), b"");
        }
    }
}

async fn dial(target: &Target, options: ConnectOptions) -> Result<Io, ConnectionEnd> {
    let name = target.name.as_str();
    match &target.transport {
        Transport::Uds(path) => {
            let stream =
                tokio::time::timeout(options.dial_timeout, tokio::net::UnixStream::connect(path))
                    .await
                    .map_err(|_| ConnectionEnd::Dropped(Some(timed_out(name))))?
                    .map_err(|error| {
                        ConnectionEnd::Dropped(Some(format!(
                            "{name}: could not connect to {}: {error}",
                            path.display()
                        )))
                    })?;
            let (reader, writer) = stream.into_split();
            Ok(Io::Stream {
                reader: Box::new(reader),
                writer: Box::new(writer),
                pending: BytesMut::with_capacity(64 * 1024),
                quic: None,
            })
        }
        Transport::Ws(url) => {
            let resolved = target.resolved().ok_or_else(|| {
                ConnectionEnd::Refused(format!("{name}: a WebSocket target needs a URL"))
            })?;
            let ws = tokio::time::timeout(options.dial_timeout, async {
                // A planning failure is configuration, which no retry
                // changes.
                let token = load_token(&resolved).map_err(ConnectionEnd::Refused)?;
                let plan = plan_ws(&resolved, url, token).map_err(ConnectionEnd::Refused)?;
                let connected =
                    phux_dial::ws::dial_with_identity(&plan, &TlsClientIdentity::None).await;
                // The Authorization header has been sent; drop the owned
                // token now.
                drop(plan);
                connected.map_err(|error| classify(name, &error))
            })
            .await
            .map_err(|_| ConnectionEnd::Dropped(Some(timed_out(name))))??;
            let (tx, rx) = ws.split();
            Ok(Io::Ws {
                reader: WsReader::new(rx),
                writer: WsWriter { tx },
            })
        }
        Transport::Quic(authority) => {
            let resolved = target.resolved().ok_or_else(|| {
                ConnectionEnd::Refused(format!("{name}: a QUIC target needs an authority"))
            })?;
            let established = tokio::time::timeout(options.dial_timeout, async {
                let plan = plan_quic(&resolved, authority)
                    .await
                    .map_err(ConnectionEnd::Refused)?;
                let connected =
                    phux_dial::quic::dial_with_identity(&plan, &TlsClientIdentity::None).await;
                // The bearer preamble has been written; the owned token goes
                // now.
                drop(plan);
                connected.map_err(|error| classify(name, &error))
            })
            .await
            .map_err(|_| ConnectionEnd::Dropped(Some(timed_out(name))))??;
            let (endpoint, connection, send, recv) = established;
            if let Some(reason) = connection.close_reason() {
                return Err(ConnectionEnd::Dropped(Some(quic_closed_message(
                    name, &reason,
                ))));
            }
            Ok(Io::Stream {
                reader: Box::new(recv),
                writer: Box::new(send),
                pending: BytesMut::with_capacity(64 * 1024),
                quic: Some((endpoint, connection)),
            })
        }
    }
}

/// A refusal no retry can satisfy ends the session; everything else walks
/// the ladder.
fn classify(name: &str, error: &phux_dial::DialError) -> ConnectionEnd {
    if is_fatal_refusal(error) {
        ConnectionEnd::Refused(dial_message(name, error))
    } else {
        ConnectionEnd::Dropped(Some(dial_message(name, error)))
    }
}

/// One connection: dial, handshake, then pump frames until it ends.
/// Returns how it ended and whether it was attached at that moment.
async fn run_connection(
    target: &Target,
    options: ConnectOptions,
    shared: &Shared,
    signals: &mut Signals,
) -> (ConnectionEnd, bool) {
    let name = target.name.as_str();
    let started = std::time::Instant::now();
    tracing::info!(host = name, transport = target.transport.label(), "dialing");
    let mut io = match dial(target, options).await {
        Ok(io) => io,
        Err(end) => return (end, false),
    };
    tracing::info!(
        host = name,
        transport = target.transport.label(),
        elapsed_ms = started.elapsed().as_millis(),
        "connected"
    );
    let end = pump(name, &mut io, options, shared, signals).await;
    let was_attached = lock(shared).status() == Status::Attached;
    io.close();
    (end, was_attached)
}

async fn pump(
    name: &str,
    io: &mut Io,
    options: ConnectOptions,
    shared: &Shared,
    signals: &mut Signals,
) -> ConnectionEnd {
    let opening = {
        let mut control = lock(shared);
        control.connection_opened();
        control.take_outbound()
    };
    if let Err(error) = write_all(io, name, opening).await {
        return ConnectionEnd::Dropped(Some(error));
    }
    // Resyncs and nudges raised while this connection was opening are
    // satisfied by the dial itself and the snapshots it is about to replay.
    signals.resync.mark_unchanged();
    signals.nudge.mark_unchanged();
    let mut probe_deadline: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
    loop {
        let expiry = lock(shared)
            .next_input_deadline()
            .map_or_else(crate::control::max_expiry_wait, |deadline| {
                deadline.saturating_duration_since(std::time::Instant::now())
            });
        tokio::select! {
            () = signals.outbound.notified() => {
                let frames = lock(shared).take_outbound();
                if let Err(error) = write_all(io, name, frames).await {
                    return ConnectionEnd::Dropped(Some(error));
                }
            }
            () = closed(&mut signals.close) => return ConnectionEnd::Closed,
            () = signal(&mut signals.resync) => return ConnectionEnd::Resync,
            () = signal(&mut signals.nudge) => {
                if let Err(error) = io.probe(name).await {
                    return ConnectionEnd::Dropped(Some(error));
                }
                if probe_deadline.is_none() {
                    probe_deadline = Some(Box::pin(tokio::time::sleep(options.probe_timeout)));
                }
            }
            () = async { if let Some(deadline) = probe_deadline.as_mut() { deadline.await } }, if probe_deadline.is_some() => {
                return ConnectionEnd::Dropped(Some("liveness probe timed out".to_owned()));
            }
            () = tokio::time::sleep(expiry) => {
                let frames = {
                    let mut control = lock(shared);
                    control.expire_inputs();
                    control.take_outbound()
                };
                if let Err(error) = write_all(io, name, frames).await {
                    return ConnectionEnd::Dropped(Some(error));
                }
            }
            inbound = io.read_frame(name) => {
                // Any inbound traffic, a pong included, proves liveness.
                probe_deadline = None;
                let frame = match inbound {
                    Ok(Some(frame)) => frame,
                    Ok(None) => return ConnectionEnd::Dropped(Some(format!("{name} closed the connection"))),
                    Err(error) => return ConnectionEnd::Dropped(Some(error)),
                };
                if frame.is_empty() {
                    continue;
                }
                let (fed, frames) = {
                    let mut control = lock(shared);
                    let fed = control.feed_bytes(&frame);
                    (fed, control.take_outbound())
                };
                if let Err(error) = write_all(io, name, frames).await {
                    return ConnectionEnd::Dropped(Some(error));
                }
                match fed {
                    Ok(()) => {}
                    Err(ControlError::Protocol(message)) => return ConnectionEnd::Dropped(Some(message)),
                    Err(ControlError::Refused(message)) => return ConnectionEnd::Refused(message),
                    Err(ControlError::Resync) => return ConnectionEnd::Resync,
                    Err(ControlError::Closed) => return ConnectionEnd::Closed,
                }
            }
        }
    }
}

async fn write_all(io: &mut Io, name: &str, frames: Vec<Vec<u8>>) -> Result<(), String> {
    for frame in frames {
        io.write_frame(name, &frame).await?;
    }
    Ok(())
}
