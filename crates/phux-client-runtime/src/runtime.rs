//! The runtime handle: one background thread runs the connection driver,
//! and a thread-safe, synchronous [`Client`] mirrors the control plane's
//! common surface for a binding to call from any thread.
//!
//! State changes reach the binding two ways: an edge-triggered
//! [`Listener`] wake (one outstanding wake no matter how many frames land;
//! [`Client::take_events`] re-arms it), and the owned batch that
//! `take_events` drains. Grid frames are acquired from the
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
        let shared: Shared = Arc::new(Mutex::new(ControlPlane::new(options.control)));
        #[cfg(feature = "engine")]
        let publication = Arc::clone(lock(&shared).publication());
        let outbound = Arc::new(Notify::new());
        let (resync_tx, resync_rx) = watch::channel(0_u64);
        let (nudge_tx, nudge_rx) = watch::channel(0_u64);
        let (close_tx, close_rx) = watch::channel(false);
        let inner = Arc::new(Inner {
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
        let weak = Arc::downgrade(&inner);
        let wake: crate::connection::Wake = Arc::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.wake();
            }
        });
        let signals = Signals {
            outbound,
            resync: resync_rx,
            nudge: nudge_rx,
            close: close_rx,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(RuntimeError::Spawn)?;
        let connect = options.connect;
        std::thread::Builder::new()
            .name("phux-client-runtime".to_owned())
            .spawn(move || {
                runtime.block_on(run_session(target, connect, shared, signals, wake));
                // A name lookup still parked on the blocking pool must not
                // hold the thread's exit hostage.
                runtime.shutdown_background();
            })
            .map_err(RuntimeError::Spawn)?;
        Ok(Client { inner })
    }
}

struct Inner {
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
    fn drop(&mut self) {
        self.shutdown();
    }
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

    /// Whether any connection ever attached.
    #[must_use]
    pub fn attached_once(&self) -> bool {
        lock(&self.inner.control).attached_once()
    }

    /// The session the active attach bootstrapped.
    #[must_use]
    pub fn attached_session(&self) -> Option<u32> {
        lock(&self.inner.control).attached_session()
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

    /// Retarget the session every connection attaches; a live connection
    /// reconnects to honor it.
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
