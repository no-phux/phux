//! The runtime handle: one background thread runs the connection driver,
//! and a thread-safe, synchronous [`Client`] mirrors the control plane's
//! common surface for a binding to call from any thread.
//!
//! State changes reach the binding two ways: an edge-triggered
//! [`Listener`] wake (one outstanding wake no matter how many frames land;
//! [`Client::take_events`] and [`Client::take_inbound`] re-arm it), and the
//! owned batch that `take_events` drains. Grid frames are acquired from the
//! `Publication` table without touching the control plane.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use phux_protocol::ResourceId;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::KeyEvent;
use phux_protocol::input::mouse::MouseEvent;
use phux_protocol::input::paste::PasteTrust;
use phux_protocol::wire::frame::{AttachTarget, Command, FrameKind};
use tokio::sync::{Notify, watch};

pub use crate::connection::{ConnectOptions, Target, Transport};
use crate::connection::{Shared, Signals, lock, run_session};
use crate::control::{
    ControlOptions, ControlPlane, Event, ServerInfo, SpawnRequest, Status, StreamRecovery, Topology,
};
#[cfg(feature = "engine")]
use crate::engine::Scroll;
#[cfg(feature = "engine")]
use crate::publication::{GridFrame, Publication, TerminalPublication};

/// A binding's wake callback: consumer-visible state changed, drain it.
/// Called from the runtime's thread, never while a lock is held.
pub trait Listener: Send + Sync {
    /// Something changed: bytes, events, status, or a frame generation.
    fn on_activity(&self);
}

/// Everything a session is configured with.
#[derive(Debug, Clone, Default)]
pub struct ClientOptions {
    /// What the control plane does on every connection.
    pub control: ControlOptions,
    /// How the driver reconnects.
    pub connect: ConnectOptions,
}

/// Which half of the runtime drives the socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// The runtime's own driver thread dials, reconnects, and pumps frames.
    Connected,
    /// The embedder owns the socket and pumps frames itself through
    /// [`Client::feed_bytes`] and [`Client::take_outbound`]. No driver
    /// thread exists, so the reconnect ladder is the embedder's problem.
    Embedded,
}

/// Why a driver-less client refused a frame pump call.
#[derive(Debug, thiserror::Error)]
pub enum PumpError {
    /// The call is only valid on an [`Lane::Embedded`] client; a connected
    /// client's driver owns the socket.
    #[error("this client's runtime driver owns the socket")]
    Connected,
    /// The control plane refused the frame.
    #[error(transparent)]
    Control(#[from] crate::control::ControlError),
}

/// Why a session could not be started.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// The background thread or its tokio runtime could not be created.
    #[error("could not start the client runtime: {0}")]
    Spawn(std::io::Error),
}

/// The entry point: [`Runtime::connect`] starts a session.
#[derive(Debug, Clone, Copy)]
pub struct Runtime;

impl Runtime {
    /// Start a session against `target` and return its client. The dial
    /// begins at once on a runtime-owned thread; observe the listener, the
    /// status, and the events.
    pub fn connect(target: Target, options: ClientOptions) -> Result<Client, RuntimeError> {
        let (inner, shared, signals) = Self::build(Lane::Connected, options.control);
        let weak = Arc::downgrade(&inner);
        let wake: crate::connection::Wake = Arc::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.wake();
            }
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(RuntimeError::Spawn)?;
        let connect = options.connect;
        let driver = std::thread::Builder::new()
            .name("phux-client-runtime".to_owned())
            .spawn(move || {
                runtime.block_on(run_session(target, connect, shared, signals, wake));
                // A name lookup still parked on the blocking pool must not
                // hold the thread's exit hostage.
                runtime.shutdown_background();
            })
            .map_err(RuntimeError::Spawn)?;
        *lock_driver(&inner) = Some(driver);
        Ok(Client { inner })
    }

    /// A session with no driver thread, for an embedder that already owns a
    /// socket: it feeds decoded frames with [`Client::feed_bytes`] and
    /// drains encoded ones with [`Client::take_outbound`]. Everything above
    /// the socket — the control plane, the engine owner thread, and the
    /// published grid — is the same as [`Runtime::connect`]'s.
    ///
    /// Nothing dials, so [`ConnectOptions`] does not apply and the reconnect
    /// ladder stays the embedder's. Prefer `connect` unless the embedder has
    /// a socket the runtime cannot own.
    #[must_use]
    pub fn embedded(options: ControlOptions) -> Client {
        // The signals are constructed and dropped: with no driver reading
        // them, `resync` and `nudge` are no-ops and `close` is observed
        // through the control plane alone.
        let (inner, _shared, _signals) = Self::build(Lane::Embedded, options);
        Client { inner }
    }

    fn build(lane: Lane, options: ControlOptions) -> (Arc<Inner>, Shared, Signals) {
        let shared: Shared = Arc::new(Mutex::new(ControlPlane::new(options)));
        #[cfg(feature = "engine")]
        let publication = Arc::clone(lock(&shared).publication());
        let outbound = Arc::new(Notify::new());
        let (resync_tx, resync_rx) = watch::channel(0_u64);
        let (nudge_tx, nudge_rx) = watch::channel(0_u64);
        let (close_tx, close_rx) = watch::channel(false);
        let inner = Arc::new(Inner {
            lane,
            driver: Mutex::new(None),
            control: Arc::clone(&shared),
            outbound: Arc::clone(&outbound),
            resync: resync_tx,
            nudge: nudge_tx,
            close: close_tx,
            listener: Mutex::new(None),
            wake_pending: AtomicBool::new(false),
            #[cfg(feature = "engine")]
            publication,
        });
        let signals = Signals {
            outbound,
            resync: resync_rx,
            nudge: nudge_rx,
            close: close_rx,
        };
        (inner, shared, signals)
    }
}

struct Inner {
    lane: Lane,
    /// The driver thread, joined on drop so no wake can outlive this
    /// client. `None` on the embedded lane, and taken by `Drop`.
    driver: Mutex<Option<std::thread::JoinHandle<()>>>,
    control: Shared,
    outbound: Arc<Notify>,
    resync: watch::Sender<u64>,
    nudge: watch::Sender<u64>,
    close: watch::Sender<bool>,
    listener: Mutex<Option<Arc<dyn Listener>>>,
    /// Edge-trigger: set while a wake has been delivered and not yet
    /// drained.
    wake_pending: AtomicBool,
    #[cfg(feature = "engine")]
    publication: Arc<Publication>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner").finish_non_exhaustive()
    }
}

impl Inner {
    /// Deliver a wake if one is not already outstanding, outside every lock.
    fn wake(&self) {
        let listener = self
            .listener
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(listener) = listener else {
            return;
        };
        if self.wake_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        listener.on_activity();
    }

    /// Run `f` on the control plane, then tell the driver about queued
    /// frames.
    fn with<T>(&self, f: impl FnOnce(&mut ControlPlane) -> T) -> T {
        let (result, queued) = {
            let mut control = lock(&self.control);
            let result = f(&mut control);
            (result, control.has_outbound())
        };
        if queued {
            self.outbound.notify_one();
        }
        result
    }

    fn shutdown(&self) {
        lock(&self.control).close();
        let _ = self.close.send(true);
    }
}

impl Drop for Inner {
    /// Closing alone would let the driver outlive this client and call a
    /// listener whose context the consumer has already freed, so the drop
    /// joins. The driver selects on the close signal, so the wait is one
    /// scheduler poll, never the network.
    fn drop(&mut self) {
        self.shutdown();
        let driver = lock_driver(self).take();
        let Some(driver) = driver else {
            return;
        };
        // The driver's wake closure upgrades a `Weak`, so for the length of
        // one callback it holds a strong reference. If the consumer drops
        // its last clone in that window, this drop runs on the driver's own
        // thread, and joining there would hang. Leaving it unjoined is safe
        // precisely then: the listener died with this `Inner`, so the thread
        // it is running on can no longer reach the consumer.
        if driver.thread().id() == std::thread::current().id() {
            return;
        }
        // A panicked driver has already published its failure through the
        // control plane; there is nothing to add here.
        let _ = driver.join();
    }
}

fn lock_driver(inner: &Inner) -> std::sync::MutexGuard<'_, Option<std::thread::JoinHandle<()>>> {
    inner.driver.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The synchronous, thread-safe handle on one session. Cloning shares the
/// session; dropping the last clone closes it.
#[derive(Debug, Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl Client {
    // ----- observation ------------------------------------------------

    /// Where the session is.
    #[must_use]
    pub fn status(&self) -> Status {
        lock(&self.inner.control).status()
    }

    /// The last failure message.
    #[must_use]
    pub fn last_error(&self) -> Option<String> {
        lock(&self.inner.control).last_error().map(str::to_owned)
    }

    /// What `HELLO_OK` negotiated on the latest connection.
    #[must_use]
    pub fn server(&self) -> Option<ServerInfo> {
        lock(&self.inner.control).server().cloned()
    }

    /// The latest session graph.
    #[must_use]
    pub fn topology(&self) -> Option<Topology> {
        lock(&self.inner.control).topology().cloned()
    }

    /// How many connections the session has opened. A consumer fences
    /// per-connection state on this once the runtime owns the reconnect.
    #[must_use]
    pub fn connection_epoch(&self) -> u64 {
        lock(&self.inner.control).connection_epoch()
    }

    /// Whether any connection ever attached.
    #[must_use]
    pub fn attached_once(&self) -> bool {
        lock(&self.inner.control).attached_once()
    }

    /// The home session whose connection-level attach opened its pumps.
    #[must_use]
    pub fn attached_session(&self) -> Option<u32> {
        lock(&self.inner.control).attached_session()
    }

    /// The session currently selected by the consumer.
    #[must_use]
    pub fn selected_session(&self) -> Option<u32> {
        lock(&self.inner.control).selected_session()
    }

    /// Which half of the runtime drives this client's socket.
    #[must_use]
    pub fn lane(&self) -> Lane {
        self.inner.lane
    }

    // ----- the embedder's frame pump ------------------------------------

    /// Feed one complete SPEC section 5 frame the embedder read off its own
    /// socket. [`Lane::Embedded`] only.
    pub fn feed_bytes(&self, bytes: &[u8]) -> Result<(), PumpError> {
        if self.inner.lane != Lane::Embedded {
            return Err(PumpError::Connected);
        }
        self.inner
            .with(|control| control.feed_bytes(bytes))
            .map_err(PumpError::Control)
    }

    /// Feed one already-decoded frame. [`Lane::Embedded`] only.
    pub fn feed(&self, frame: FrameKind) -> Result<(), PumpError> {
        if self.inner.lane != Lane::Embedded {
            return Err(PumpError::Connected);
        }
        self.inner
            .with(|control| control.feed(frame))
            .map_err(PumpError::Control)
    }

    /// Drain the frames a connected driver retained under
    /// [`InboundDelivery::Queued`](crate::control::InboundDelivery::Queued),
    /// for the consumer to feed on its own thread. This acknowledges the
    /// queued activity and re-arms the listener before the snapshot, so a
    /// frame queued concurrently cannot be stranded behind its wake.
    #[must_use]
    pub fn take_inbound(&self) -> Vec<Vec<u8>> {
        self.inner.wake_pending.store(false, Ordering::Release);
        lock(&self.inner.control).take_inbound()
    }

    /// Whether a drain would find anything: a retained inbound frame, or an
    /// event the consumer has not taken. A consumer that polls rather than
    /// waiting on the listener uses this to skip an empty turn.
    #[must_use]
    pub fn poll_pending(&self) -> bool {
        let control = lock(&self.inner.control);
        control.has_inbound() || control.has_events()
    }

    /// Take the encoded frames the control plane has queued, for the
    /// embedder to write to its own socket. [`Lane::Embedded`] only; a
    /// connected client's driver drains them instead.
    #[must_use]
    pub fn take_outbound(&self) -> Vec<Vec<u8>> {
        if self.inner.lane != Lane::Embedded {
            return Vec::new();
        }
        lock(&self.inner.control).take_outbound()
    }

    // ----- the binding's escape hatch -----------------------------------

    /// Run `f` against the control plane, then tell the driver about any
    /// frames it queued.
    ///
    /// A binding reaches for this only where the common surface above has
    /// no equivalent. It is the one place the lock is held for binding
    /// code, so `f` must not block, call back into this `Client`, or
    /// otherwise take another runtime lock.
    pub fn with_control<T>(&self, f: impl FnOnce(&mut ControlPlane) -> T) -> T {
        self.inner.with(f)
    }

    /// Borrow the control plane directly, for a binding whose call shape
    /// does not fit a closure. Releasing the guard tells the driver about
    /// any frames the borrow queued, exactly as [`Client::with_control`]
    /// does.
    ///
    /// The lock is not reentrant: hold at most one guard per thread, and
    /// do not call back into this `Client` while one is alive.
    #[must_use]
    pub fn control(&self) -> ControlGuard<'_> {
        ControlGuard {
            guard: lock(&self.inner.control),
            inner: &self.inner,
        }
    }

    /// Deliver a wake to the listener if one is not already outstanding.
    /// A binding calls this after work that changed consumer-visible state
    /// without queueing a frame.
    pub fn wake(&self) {
        self.inner.wake();
    }

    // ----- events -------------------------------------------------------

    /// Drain every event since the last drain and re-arm the wake.
    #[must_use]
    pub fn take_events(&self) -> Vec<Event> {
        self.inner.wake_pending.store(false, Ordering::Release);
        lock(&self.inner.control).take_events()
    }

    /// Install the wake callback; one listener, a new one replaces the old.
    /// An immediate wake covers anything that landed before registration.
    pub fn set_listener(&self, listener: Arc<dyn Listener>) {
        *self
            .inner
            .listener
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(listener);
        self.inner.wake();
    }

    // ----- lifecycle -------------------------------------------------

    /// Select a session. A healthy connection switches with per-terminal
    /// subscriptions on the same socket; only an unresolved create target
    /// requires a resync.
    pub fn attach_session(&self, target: AttachTarget) {
        if self.inner.with(|control| control.attach_session(target)) {
            self.resync();
        }
    }

    /// Ask the server to end the attach; the status becomes `Closed` when
    /// it answers.
    pub fn detach(&self) {
        self.inner.with(ControlPlane::detach);
    }

    /// Drop the socket and redial for fresh snapshots. A no-op once the
    /// session is closed or failed.
    pub fn resync(&self) {
        self.inner.resync.send_modify(|n| *n += 1);
    }

    /// Cut a reconnect backoff short, or probe an attached socket that may
    /// have gone stale (app foreground, network path change).
    pub fn nudge(&self) {
        self.inner.nudge.send_modify(|n| *n += 1);
    }

    /// End the session for good; idempotent, and there is no reopen.
    pub fn close(&self) {
        self.inner.shutdown();
        self.inner.wake();
    }

    /// The client's viewport changed.
    pub fn resize_viewport(&self, cols: u16, rows: u16) {
        self.inner
            .with(|control| control.resize_viewport(cols, rows));
    }

    // ----- topology and terminals ------------------------------------

    /// Re-read the session graph on the live socket; `None` before
    /// `HELLO_OK`.
    #[must_use]
    pub fn refresh_topology(&self) -> Option<u32> {
        self.inner.with(ControlPlane::refresh_topology)
    }

    /// Subscribe to one terminal's stream on the live socket.
    #[must_use]
    pub fn attach_terminal(&self, terminal_id: &ResourceId) -> u32 {
        self.inner
            .with(|control| control.attach_terminal(terminal_id))
    }

    /// Drop a per-terminal subscription.
    #[must_use]
    pub fn detach_terminal(&self, terminal_id: &ResourceId) -> u32 {
        self.inner
            .with(|control| control.detach_terminal(terminal_id))
    }

    /// Make one terminal's stream live again, reconnecting when only a
    /// fresh snapshot can help.
    #[must_use]
    pub fn ensure_stream(&self, terminal_id: &ResourceId) -> StreamRecovery {
        let recovery = self
            .inner
            .with(|control| control.ensure_stream(terminal_id));
        if recovery == StreamRecovery::Reconnect {
            self.resync();
        }
        recovery
    }

    /// Spawn a terminal; the reply is [`Event::TerminalSpawned`].
    #[must_use]
    pub fn spawn_terminal(&self, request: SpawnRequest) -> u32 {
        self.inner.with(|control| control.spawn_terminal(request))
    }

    /// Terminate a terminal's process.
    #[must_use]
    pub fn kill_terminal(&self, terminal_id: &ResourceId) -> u32 {
        self.inner
            .with(|control| control.kill_terminal(terminal_id))
    }

    /// Close a batch of terminals atomically; `None` when the server does
    /// not support it.
    #[must_use]
    pub fn close_terminals(&self, ids: Vec<ResourceId>) -> Option<u32> {
        self.inner.with(|control| control.close_terminals(ids))
    }
}

/// A live borrow of the control plane, handed to a binding by
/// [`Client::control`]. Dropping it notifies the driver about queued frames.
pub struct ControlGuard<'a> {
    guard: std::sync::MutexGuard<'a, ControlPlane>,
    inner: &'a Inner,
}

impl std::ops::Deref for ControlGuard<'_> {
    type Target = ControlPlane;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl std::ops::DerefMut for ControlGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl std::fmt::Debug for ControlGuard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlGuard").finish_non_exhaustive()
    }
}

impl Drop for ControlGuard<'_> {
    /// `Drop::drop` runs before the guard field it owns, so the notify
    /// below happens with the control lock still held. That costs the
    /// driver one momentary contention on a lock this thread is about to
    /// release, and it buys a guard with no `Option` and no panic path.
    /// `Notify::notify_one` only stores a permit or wakes a waker; it
    /// never takes a runtime lock, so it cannot deadlock against us.
    fn drop(&mut self) {
        if self.guard.has_outbound() {
            self.inner.outbound.notify_one();
        }
    }
}

mod extensions;
mod input;
mod projection;

impl Client {
    /// The engine host, for a binding that needs an operation the common
    /// surface does not expose; `None` before `HELLO_OK`.
    #[must_use]
    pub fn engine(&self) -> Option<crate::engine::EngineHandle> {
        lock(&self.inner.control).engine().cloned()
    }
}
