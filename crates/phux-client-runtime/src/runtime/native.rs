//! Native construction, driver lifetime, and synchronous control handoff.

use super::*;
use super::wakes::Wakes;

impl Runtime {
    /// Start a session against `target` and return its client. The dial
    /// begins at once on a runtime-owned thread; observe the listener, the
    /// status, and the events.
    pub fn connect(target: Target, options: ClientOptions) -> Result<Client, RuntimeError> {
        let (inner, shared, signals) = Self::build(Lane::Connected, options.control);
        // The driver shares only the wake slot, never the client, so the
        // consumer's last drop always runs on the consumer's thread.
        let wakes = Arc::clone(&inner.wakes);
        let wake: crate::connection::Wake = Arc::new(move || wakes.wake());
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
        let shared: Shared = Arc::new(Mutex::new(ControlPlane::new(std::time::Instant::now(), options)));
        #[cfg(feature = "engine")]
        let publication = Arc::clone(lock(&shared).publication());
        let outbound = Arc::new(Notify::new());
        let (resync_tx, resync_rx) = watch::channel(0_u64);
        let (nudge_tx, nudge_rx) = watch::channel(0_u64);
        let (rebind_tx, rebind_rx) = watch::channel(0_u64);
        let (close_tx, close_rx) = watch::channel(false);
        let trace = Arc::new(Mutex::new(None));
        let inner = Arc::new(Inner {
            lane,
            driver: Mutex::new(None),
            control: Arc::clone(&shared),
            outbound: Arc::clone(&outbound),
            resync: resync_tx,
            nudge: nudge_tx,
            rebind: rebind_tx,
            close: close_tx,
            wakes: Arc::new(Wakes::default()),
            trace: Arc::clone(&trace),
            #[cfg(feature = "engine")]
            publication,
        });
        let signals = Signals {
            trace,
            outbound,
            resync: resync_rx,
            nudge: nudge_rx,
            rebind: rebind_rx,
            close: close_rx,
        };
        (inner, shared, signals)
    }
}

pub(super) struct Inner {
    pub(super) lane: Lane,
    /// The driver thread, joined on drop so no wake can outlive this
    /// client. `None` on the embedded lane, and taken by `Drop`.
    pub(super) driver: Mutex<Option<std::thread::JoinHandle<()>>>,
    pub(super) control: Shared,
    pub(super) outbound: Arc<Notify>,
    pub(super) resync: watch::Sender<u64>,
    pub(super) nudge: watch::Sender<u64>,
    pub(super) rebind: watch::Sender<u64>,
    pub(super) close: watch::Sender<bool>,
    pub(super) wakes: Arc<Wakes>,
    pub(super) trace: Arc<Mutex<Option<crate::connection::AttemptTrace>>>,
    #[cfg(feature = "engine")]
    pub(super) publication: Arc<Publication>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner").finish_non_exhaustive()
    }
}

impl Inner {
    pub(super) fn wake(&self) {
        self.wakes.wake();
    }

    /// Run `f` on the control plane, then tell the driver about queued
    /// frames.
    pub(super) fn with<T>(&self, f: impl FnOnce(&mut ControlPlane) -> T) -> T {
        let (result, queued) = {
            let mut control = lock(&self.control);
            control.observe_time(std::time::Instant::now());
            let result = f(&mut control);
            (result, control.has_outbound())
        };
        if queued {
            self.outbound.notify_one();
        }
        result
    }

    pub(super) fn shutdown(&self) {
        let mut control = lock(&self.control);
        control.observe_time(std::time::Instant::now());
        control.close();
        drop(control);
        let _ = self.close.send(true);
    }
}

impl Drop for Inner {
    /// Closing alone would let the driver outlive this client and call a
    /// listener whose context the consumer has already freed, so the drop
    /// retires the listener, waiting out a callback in flight, and joins.
    /// The driver selects on the close signal, so the wait is one scheduler
    /// poll, never the network.
    fn drop(&mut self) {
        self.shutdown();
        self.wakes.retire();
        let driver = lock_driver(self).take();
        let Some(driver) = driver else {
            return;
        };
        // A listener that drops the client from inside its own callback
        // runs this on the driver's thread, which cannot join itself; with
        // the listener retired, the driver can no longer reach the consumer.
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

