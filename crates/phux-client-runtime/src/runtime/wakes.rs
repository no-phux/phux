//! Edge-triggered native listener delivery and callback retirement.

use super::{Arc, AtomicBool, Cell, Condvar, Listener, Mutex, Ordering, PoisonError};

/// The listener and its edge trigger. The driver's wake closure holds this,
/// not the client, and no lock is held while the listener runs.
#[derive(Default)]
pub(super) struct Wakes {
    slot: Mutex<WakeSlot>,
    /// Signalled when the last in-flight callback returns.
    idle: Condvar,
    /// Edge-trigger: set while a wake has been delivered and not yet
    /// drained.
    pub(super) pending: AtomicBool,
}

#[derive(Default)]
struct WakeSlot {
    listener: Option<Arc<dyn Listener>>,
    /// Callbacks running now, on any thread.
    in_flight: usize,
}

thread_local! {
    /// Callbacks running on this thread, so a client dropped from inside
    /// its own callback does not wait for itself.
    static CALLBACKS_HERE: Cell<usize> = const { Cell::new(0) };
}

impl Wakes {
    fn lock(&self) -> std::sync::MutexGuard<'_, WakeSlot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Deliver a wake if one is not already outstanding, outside every lock.
    pub(super) fn wake(&self) {
        let listener = {
            let mut slot = self.lock();
            let Some(listener) = slot.listener.clone() else {
                return;
            };
            if self.pending.swap(true, Ordering::AcqRel) {
                return;
            }
            slot.in_flight += 1;
            listener
        };
        let _running = InFlight::new(self);
        listener.on_activity();
    }

    pub(super) fn set(&self, listener: Arc<dyn Listener>) {
        self.lock().listener = Some(listener);
    }

    /// Deliver no further wake, and return only once every callback in
    /// flight on another thread has returned, so the embedder may free the
    /// listener's context. One running on this thread is the caller's own.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the condvar wait consumes the guard"
    )]
    pub(super) fn retire(&self) {
        let own = CALLBACKS_HERE.with(Cell::get);
        let mut slot = self.lock();
        slot.listener = None;
        let _idle = self
            .idle
            .wait_while(slot, |slot| slot.in_flight > own)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

/// One running callback, counted until it returns or unwinds.
struct InFlight<'a>(&'a Wakes);

impl<'a> InFlight<'a> {
    fn new(wakes: &'a Wakes) -> Self {
        CALLBACKS_HERE.with(|here| here.set(here.get() + 1));
        Self(wakes)
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        CALLBACKS_HERE.with(|here| here.set(here.get() - 1));
        let mut slot = self.0.lock();
        slot.in_flight -= 1;
        if slot.in_flight == 0 {
            self.0.idle.notify_all();
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod wake_tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// Parks its first callback until released; counts every call.
    struct Parked {
        calls: AtomicUsize,
        entered: Mutex<Option<mpsc::Sender<()>>>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl Listener for Parked {
        fn on_activity(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let entered = self.entered.lock().expect("entered").take();
            if let Some(entered) = entered {
                entered.send(()).expect("test waits");
                self.release
                    .lock()
                    .expect("release")
                    .recv()
                    .expect("released");
            }
        }
    }

    /// The use-after-free behind freeing a client: retiring the listener
    /// returns only after a callback already running elsewhere returns, and
    /// no wake reaches the listener afterwards.
    #[test]
    fn retire_waits_out_a_callback_in_flight_and_ends_wakes() {
        let wakes = Arc::new(Wakes::default());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let listener = Arc::new(Parked {
            calls: AtomicUsize::new(0),
            entered: Mutex::new(Some(entered_tx)),
            release: Mutex::new(release_rx),
        });
        wakes.set(listener.clone());

        let driver = std::thread::spawn({
            let wakes = Arc::clone(&wakes);
            move || wakes.wake()
        });
        entered_rx.recv().expect("callback running");

        let (retired_tx, retired_rx) = mpsc::channel();
        let consumer = std::thread::spawn({
            let wakes = Arc::clone(&wakes);
            move || {
                wakes.retire();
                retired_tx.send(()).expect("test waits");
            }
        });
        assert!(
            retired_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "retire returned while the callback was still running"
        );
        release_tx.send(()).expect("callback waits");
        retired_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("retire returns once the callback does");
        driver.join().expect("driver");
        consumer.join().expect("consumer");

        wakes.pending.store(false, Ordering::Release);
        wakes.wake();
        assert_eq!(
            listener.calls.load(Ordering::SeqCst),
            1,
            "no wake after retire"
        );
    }

    /// Retiring from inside the callback, as a client dropped by its own
    /// listener does, cannot wait for itself.
    #[test]
    fn retire_from_inside_the_callback_does_not_wait_for_itself() {
        struct Retiring(Arc<Wakes>);
        impl Listener for Retiring {
            fn on_activity(&self) {
                self.0.retire();
            }
        }
        let wakes = Arc::new(Wakes::default());
        wakes.set(Arc::new(Retiring(Arc::clone(&wakes))));
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            wakes.wake();
            done_tx.send(()).expect("test waits");
        });
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a self-retiring callback returns");
    }
}
