//! One poller for quiet PTY masters.
//!
//! A pane that is writing gets a dedicated reader thread, so a flood cannot
//! head-of-line-block every other pane. After [`QUIET_AFTER`] with no bytes
//! that thread registers the master here and exits. Fresh panes start
//! registered, so a few thousand idle shells cost one thread, not one each.
//! Linux waits with epoll; macOS waits with kqueue.
//!
//! Each wake keeps reading until the next call would block or 64 KiB is
//! queued, then hands that batch to the actor once. A short read is not held
//! for a timer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use tokio::sync::mpsc::{self, error::TrySendError};

use super::spawn::{
    PTY_READ_BATCH, PTY_READ_CHUNK, PtyEvent, drain_master_with_budget, send_pty_chunk,
};

/// How long a dedicated reader waits for the next byte before giving its fd
/// back to the shared poller.
const QUIET_AFTER: Duration = Duration::from_secs(2);
/// Upper bound on how long `shutdown` waits inside a hot reader's `poll`.
const CANCEL_SLICE: Duration = Duration::from_millis(200);
/// `udata` / epoll data of the self-pipe. Real panes start at 1.
const WAKE_ID: u64 = 0;
pub(super) const READER_STACK: usize = 256 * 1024;

static PROMOTE_COUNT: AtomicU64 = AtomicU64::new(0);

/// Handle the actor uses to stop a pane's reader, whether it is parked or hot.
#[derive(Debug)]
pub(super) struct PtyReader {
    inner: Option<ReaderInner>,
}

#[derive(Debug)]
enum ReaderInner {
    Shared(u64),
    Dedicated(JoinHandle<()>),
}

impl PtyReader {
    /// Stop the reader. A parked fd is closed now. A hot thread is asked to
    /// exit; the returned handle is what [`join`](std::thread::JoinHandle::join)
    /// waits on.
    pub(super) fn shutdown(&mut self) -> Option<JoinHandle<()>> {
        match self.inner.take() {
            Some(ReaderInner::Shared(id)) => shared_shutdown(id),
            Some(ReaderInner::Dedicated(handle)) => Some(handle),
            None => None,
        }
    }

    /// Like [`Self::shutdown`], but do not join. Used when the child is
    /// already gone and the actor must not block.
    pub(super) fn detach(&mut self) {
        drop(self.shutdown());
    }
}

impl Drop for PtyReader {
    fn drop(&mut self) {
        self.detach();
    }
}

/// Where one reader sits. Per-reader, so a test can ignore panes other tests
/// still have registered on the process-wide poller.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReaderPlace {
    /// Registered with the shared poller.
    Parked,
    /// A dedicated thread is reading this pane.
    Hot,
    /// The shared poller was unavailable, so this pane has its own thread.
    Dedicated,
    /// The poller took the fd for one wake.
    Reading,
    /// The slot is already gone.
    Gone,
}

/// Place of `reader`. [`ReaderPlace::Dedicated`] when it never joined the poller.
#[cfg(test)]
pub(super) fn reader_place(reader: &PtyReader) -> ReaderPlace {
    match reader.inner.as_ref() {
        Some(ReaderInner::Dedicated(_)) => ReaderPlace::Dedicated,
        Some(ReaderInner::Shared(id)) => shared_park()
            .and_then(|park| park.place(*id))
            .unwrap_or(ReaderPlace::Gone),
        None => ReaderPlace::Gone,
    }
}

/// Times a quiet pane was promoted onto a dedicated reader.
#[cfg(test)]
pub(super) fn promote_count() -> u64 {
    PROMOTE_COUNT.load(Ordering::Relaxed)
}

/// Read `file` until the actor drops the channel. Prefers the shared poller.
pub(super) fn attach(file: File, tx: mpsc::Sender<PtyEvent>, drain_budget: Duration) -> PtyReader {
    let file = PtyMaster { file, drain_budget };
    match shared_park() {
        Some(park) => park.deposit(file, tx),
        None => PtyReader {
            inner: Some(ReaderInner::Dedicated(spawn_dedicated(file, tx))),
        },
    }
}

fn shared_park() -> Option<&'static Park> {
    static SLOT: OnceLock<Option<Park>> = OnceLock::new();
    let park = SLOT
        .get_or_init(|| match Park::start() {
            Ok(park) => Some(park),
            Err(err) => {
                tracing::warn!(
                    ?err,
                    "pty poller unavailable; each pane keeps a reader thread"
                );
                None
            }
        })
        .as_ref()?;
    boot_poller(park);
    Some(park)
}

fn shared_shutdown(id: u64) -> Option<JoinHandle<()>> {
    shared_park().and_then(|park| park.shutdown(id))
}

struct Park {
    reactor: Reactor,
    wake_read: OwnedFd,
    wake_write: OwnedFd,
    slots: Mutex<HashMap<u64, Slot>>,
    next_id: AtomicU64,
}

struct Slot {
    state: SlotState,
    cancel: std::sync::Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

enum SlotState {
    Parked(ParkedPty),
    /// The poller took the file out to read it.
    Reading,
    Hot,
}

struct ParkedPty {
    file: PtyMaster,
    tx: mpsc::Sender<PtyEvent>,
}

impl Park {
    fn start() -> io::Result<Self> {
        let (wake_read, wake_write) = nix::unistd::pipe().map_err(io::Error::from)?;
        set_cloexec_nonblock(&wake_read)?;
        set_cloexec_nonblock(&wake_write)?;
        let reactor = Reactor::start()?;
        reactor.add(wake_read.as_fd(), WAKE_ID)?;
        let park = Self {
            reactor,
            wake_read,
            wake_write,
            slots: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        };
        // The thread is spawned by `shared_park` after the OnceLock is filled,
        // because it needs `&'static Park`. `start` only builds the fds.
        Ok(park)
    }

    fn boot(&'static self) {
        let spawned = std::thread::Builder::new()
            .name("phux-pty-park".to_owned())
            .stack_size(READER_STACK)
            .spawn(move || {
                phux_resource::perf::promote_helper_thread("phux-pty-park");
                self.poll_loop();
            });
        if let Err(err) = spawned {
            tracing::error!(?err, "pty poller thread failed to start");
        }
    }

    fn deposit(&'static self, file: PtyMaster, tx: mpsc::Sender<PtyEvent>) -> PtyReader {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Err(err) = self.reactor.add(file.as_fd(), id) {
            tracing::warn!(?err, "parking a pty failed; using a dedicated reader");
            return PtyReader {
                inner: Some(ReaderInner::Dedicated(spawn_dedicated(file, tx))),
            };
        }
        self.slots().insert(
            id,
            Slot {
                state: SlotState::Parked(ParkedPty { file, tx }),
                cancel: std::sync::Arc::new(AtomicBool::new(false)),
                join: None,
            },
        );
        self.poke();
        PtyReader {
            inner: Some(ReaderInner::Shared(id)),
        }
    }

    fn shutdown(&self, id: u64) -> Option<JoinHandle<()>> {
        let mut slots = self.slots();
        let slot = slots.get_mut(&id)?;
        slot.cancel.store(true, Ordering::Release);
        match &slot.state {
            SlotState::Parked(parked) => {
                self.reactor.delete(parked.file.as_fd());
                slots.remove(&id);
                None
            }
            SlotState::Reading | SlotState::Hot => slot.join.take(),
        }
    }

    #[cfg(test)]
    fn place(&self, id: u64) -> Option<ReaderPlace> {
        let slots = self.slots();
        let place = slots.get(&id).map(|slot| match &slot.state {
            SlotState::Parked(_) => ReaderPlace::Parked,
            SlotState::Hot => ReaderPlace::Hot,
            SlotState::Reading => ReaderPlace::Reading,
        });
        drop(slots);
        place
    }

    fn poll_loop(&'static self) {
        loop {
            let ready = match self.reactor.wait() {
                Ok(ready) => ready,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    tracing::error!(?err, "pty poller wait failed");
                    std::thread::sleep(CANCEL_SLICE);
                    continue;
                }
            };
            for id in ready {
                self.on_ready(id);
            }
        }
    }

    fn on_ready(&'static self, id: u64) {
        if id == WAKE_ID {
            drain_fd(&self.wake_read);
            return;
        }
        let Some(mut job) = self.take_for_read(id) else {
            return;
        };
        if job.cancel.load(Ordering::Acquire) {
            self.remove(id);
            return;
        }
        match read_once(&mut job.file, &job.tx) {
            ReadStep::Promote(pending) => self.spawn_hot(id, job, pending),
            ReadStep::Eof => {
                self.reactor.delete(job.file.as_fd());
                self.remove(id);
            }
            ReadStep::Closed => {
                self.reactor.delete(job.file.as_fd());
                detach_drain(job.file);
                self.remove(id);
            }
            ReadStep::Retry => self.put_back(id, job),
        }
    }

    fn take_for_read(&self, id: u64) -> Option<ReadJob> {
        let mut slots = self.slots();
        let slot = slots.get_mut(&id)?;
        if slot.cancel.load(Ordering::Acquire) {
            if let SlotState::Parked(parked) = &slot.state {
                self.reactor.delete(parked.file.as_fd());
            }
            slots.remove(&id);
            return None;
        }
        let SlotState::Parked(parked) = std::mem::replace(&mut slot.state, SlotState::Reading)
        else {
            return None;
        };
        let cancel = std::sync::Arc::clone(&slot.cancel);
        drop(slots);
        Some(ReadJob {
            file: parked.file,
            tx: parked.tx,
            cancel,
        })
    }

    fn put_back(&self, id: u64, job: ReadJob) {
        let mut slots = self.slots();
        let Some(slot) = slots.get_mut(&id) else {
            return;
        };
        if slot.cancel.load(Ordering::Acquire) {
            self.reactor.delete(job.file.as_fd());
            slots.remove(&id);
            return;
        }
        slot.state = SlotState::Parked(ParkedPty {
            file: job.file,
            tx: job.tx,
        });
        drop(slots);
    }

    fn spawn_hot(&'static self, id: u64, job: ReadJob, pending: Vec<PtyEvent>) {
        self.reactor.delete(job.file.as_fd());
        let ready = std::sync::Arc::new(AtomicBool::new(false));
        let ready_for_thread = std::sync::Arc::clone(&ready);
        let cancel = std::sync::Arc::clone(&job.cancel);
        let file = job.file;
        let tx = job.tx;
        let handle = std::thread::Builder::new()
            .name("phux-pty-reader".to_owned())
            .stack_size(READER_STACK)
            .spawn(move || {
                while !ready_for_thread.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                phux_resource::perf::promote_helper_thread("phux-pty-reader");
                hot_loop(self, id, file, tx, cancel, pending);
            });
        {
            let mut slots = self.slots();
            let Some(slot) = slots.get_mut(&id) else {
                drop(slots);
                ready.store(true, Ordering::Release);
                return;
            };
            match handle {
                Ok(handle) => {
                    slot.state = SlotState::Hot;
                    slot.join = Some(handle);
                    PROMOTE_COUNT.fetch_add(1, Ordering::Relaxed);
                }
                Err(err) => {
                    tracing::warn!(?err, "dedicated pty reader failed to start");
                    slots.remove(&id);
                }
            }
        }
        ready.store(true, Ordering::Release);
    }

    fn remove(&self, id: u64) {
        self.slots().remove(&id);
    }

    fn poke(&self) {
        let _ = nix::unistd::write(&self.wake_write, &[1u8]);
    }

    fn slots(&self) -> MutexGuard<'_, HashMap<u64, Slot>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod coalesce_tests;

mod hot;
mod reactor;
mod read;

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "re-export for siblings"
)]
use hot::*;
#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "re-export for siblings"
)]
use reactor::*;
#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "re-export for siblings"
)]
pub(super) use read::*;
