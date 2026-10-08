//! One poller for quiet PTY masters.
//!
//! A pane that is writing gets a dedicated reader thread, so a flood cannot
//! head-of-line-block every other pane. After [`QUIET_AFTER`] with no bytes
//! that thread registers the master here and exits. Fresh panes start
//! registered, so a few thousand idle shells cost one thread, not one each.
//! Linux waits with epoll; macOS waits with kqueue.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{self, error::TrySendError};

use super::spawn::{PTY_READ_CHUNK, PtyEvent, drain_master_to_eof, send_pty_chunk};

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

/// Panes currently registered with the shared poller.
#[cfg(test)]
pub(super) fn parked_count() -> usize {
    shared_park().map_or(0, |park| park.count(SlotKind::Parked))
}

/// Panes whose output is being read by a dedicated thread.
#[cfg(test)]
pub(super) fn hot_count() -> usize {
    shared_park().map_or(0, |park| park.count(SlotKind::Hot))
}

/// Times a quiet pane was promoted onto a dedicated reader.
#[cfg(test)]
pub(super) fn promote_count() -> u64 {
    PROMOTE_COUNT.load(Ordering::Relaxed)
}

/// Read `file` until the actor drops the channel. Prefers the shared poller.
pub(super) fn attach(file: File, tx: mpsc::Sender<PtyEvent>) -> PtyReader {
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

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotKind {
    Parked,
    Hot,
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
    file: File,
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
                crate::perf::promote_helper_thread("phux-pty-park");
                self.poll_loop();
            });
        if let Err(err) = spawned {
            tracing::error!(?err, "pty poller thread failed to start");
        }
    }

    fn deposit(&'static self, file: File, tx: mpsc::Sender<PtyEvent>) -> PtyReader {
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
    fn count(&self, kind: SlotKind) -> usize {
        self.slots()
            .values()
            .filter(|slot| {
                matches!(
                    (&slot.state, kind),
                    (SlotState::Parked(_), SlotKind::Parked) | (SlotState::Hot, SlotKind::Hot)
                )
            })
            .count()
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
                crate::perf::promote_helper_thread("phux-pty-reader");
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

struct ReadJob {
    file: File,
    tx: mpsc::Sender<PtyEvent>,
    cancel: std::sync::Arc<AtomicBool>,
}

enum ReadStep {
    /// Bytes are flowing. A dedicated thread should take over.
    Promote(Vec<PtyEvent>),
    Eof,
    /// The actor is gone. The caller drops the file; a drain thread is spawned
    /// when the child may still be writing.
    Closed,
    /// No byte was consumed. Put the fd back on the poller.
    Retry,
}

fn read_once(file: &mut File, tx: &mpsc::Sender<PtyEvent>) -> ReadStep {
    let mut buf = vec![0_u8; PTY_READ_CHUNK];
    loop {
        match file.read(&mut buf) {
            Ok(0) => return deliver_terminal(tx, PtyEvent::Eof),
            Ok(n) => {
                note_read(n);
                let chunk = bytes::Bytes::copy_from_slice(&buf[..n]);
                return match tx.try_send(PtyEvent::Bytes {
                    chunk,
                    read_at: Instant::now(),
                }) {
                    Ok(()) => ReadStep::Promote(Vec::new()),
                    Err(TrySendError::Full(event)) => ReadStep::Promote(vec![event]),
                    Err(TrySendError::Closed(_)) => ReadStep::Closed,
                };
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) if err.raw_os_error() == Some(super::spawn::EIO) => {
                return deliver_terminal(tx, PtyEvent::Eof);
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return ReadStep::Retry,
            Err(err) => {
                tracing::debug!(?err, "pty poller read error");
                return deliver_terminal(tx, PtyEvent::Eof);
            }
        }
    }
}

fn deliver_terminal(tx: &mpsc::Sender<PtyEvent>, event: PtyEvent) -> ReadStep {
    match tx.try_send(event) {
        Ok(()) => ReadStep::Eof,
        Err(TrySendError::Full(event)) => ReadStep::Promote(vec![event]),
        Err(TrySendError::Closed(_)) => ReadStep::Closed,
    }
}

fn detach_drain(mut file: File) {
    let spawned = std::thread::Builder::new()
        .name("phux-pty-drain".to_owned())
        .stack_size(READER_STACK)
        .spawn(move || {
            let mut buf = vec![0_u8; PTY_READ_CHUNK];
            drain_master_to_eof(&mut file, &mut buf);
        });
    if let Err(err) = spawned {
        tracing::warn!(?err, "pty orphan drain thread failed to start");
    }
}

fn note_read(n: usize) {
    crate::perf::PTY_READ_SIZE.record_len(n);
    crate::perf::PTY_READ_BYTES.add_len(n);
    tracing::debug!(n, "pty read");
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the reader thread keeps the cancel flag alive after its slot is removed"
)]
fn hot_loop(
    park: &'static Park,
    id: u64,
    mut file: File,
    mut tx: mpsc::Sender<PtyEvent>,
    cancel: std::sync::Arc<AtomicBool>,
    pending: Vec<PtyEvent>,
) {
    if !flush_pending(&tx, &mut file, pending) {
        park.remove(id);
        return;
    }
    let mut buf = vec![0_u8; PTY_READ_CHUNK];
    let mut last = Instant::now();
    loop {
        if cancel.load(Ordering::Acquire) {
            park.remove(id);
            return;
        }
        let idle = last.elapsed();
        if idle >= QUIET_AFTER {
            match try_repark(park, id, file, tx) {
                Repark::Parked => return,
                Repark::Keep(kept_file, kept_tx) => {
                    file = kept_file;
                    tx = kept_tx;
                    last = Instant::now();
                }
            }
        }
        let slice = CANCEL_SLICE.min(QUIET_AFTER.saturating_sub(idle));
        match poll_readable(&file, slice) {
            PollRead::Timeout => {}
            PollRead::Ready => {
                if handle_hot_read(park, id, &mut file, &tx, &mut buf, &mut last).is_exit() {
                    return;
                }
            }
            PollRead::Hungup => {
                let _ = tx.blocking_send(PtyEvent::Eof);
                park.remove(id);
                return;
            }
        }
    }
}

enum AfterRead {
    Stay,
    Exit,
}

impl AfterRead {
    const fn is_exit(&self) -> bool {
        matches!(self, Self::Exit)
    }
}

fn handle_hot_read(
    park: &'static Park,
    id: u64,
    file: &mut File,
    tx: &mpsc::Sender<PtyEvent>,
    buf: &mut [u8],
    last: &mut Instant,
) -> AfterRead {
    match file.read(buf) {
        Ok(0) => finish_hot(park, id, tx, PtyEvent::Eof),
        Ok(n) => {
            *last = Instant::now();
            note_read(n);
            let chunk = bytes::Bytes::copy_from_slice(&buf[..n]);
            if send_pty_chunk(tx, chunk, *last).is_break() {
                drain_master_to_eof(file, buf);
                park.remove(id);
                AfterRead::Exit
            } else {
                AfterRead::Stay
            }
        }
        Err(err) if err.kind() == io::ErrorKind::Interrupted => AfterRead::Stay,
        Err(err) if err.raw_os_error() == Some(super::spawn::EIO) => {
            finish_hot(park, id, tx, PtyEvent::Eof)
        }
        Err(err) => {
            tracing::debug!(?err, "pty reader thread: read error");
            finish_hot(park, id, tx, PtyEvent::Eof)
        }
    }
}

fn finish_hot(park: &Park, id: u64, tx: &mpsc::Sender<PtyEvent>, event: PtyEvent) -> AfterRead {
    let _ = tx.blocking_send(event);
    park.remove(id);
    AfterRead::Exit
}

/// `false` when the actor is gone and the master has been drained.
fn flush_pending(tx: &mpsc::Sender<PtyEvent>, file: &mut File, pending: Vec<PtyEvent>) -> bool {
    for event in pending {
        match event {
            PtyEvent::Bytes { chunk, read_at } => {
                if send_pty_chunk(tx, chunk, read_at).is_break() {
                    let mut buf = vec![0_u8; PTY_READ_CHUNK];
                    drain_master_to_eof(file, &mut buf);
                    return false;
                }
            }
            PtyEvent::Eof => {
                let _ = tx.blocking_send(PtyEvent::Eof);
                return false;
            }
        }
    }
    true
}

enum Repark {
    /// Fd is on the poller, or the pane is gone. The reader thread exits.
    Parked,
    /// Stay on this thread; the poller refused the fd.
    Keep(File, mpsc::Sender<PtyEvent>),
}

/// Park `file`. On cancel the slot is removed and `file` is dropped here.
fn try_repark(park: &'static Park, id: u64, file: File, tx: mpsc::Sender<PtyEvent>) -> Repark {
    let mut slots = park.slots();
    let Some(slot) = slots.get_mut(&id) else {
        return Repark::Parked;
    };
    if slot.cancel.load(Ordering::Acquire) {
        park.reactor.delete(file.as_fd());
        slots.remove(&id);
        return Repark::Parked;
    }
    if let Err(err) = park.reactor.add(file.as_fd(), id) {
        tracing::warn!(?err, "returning a quiet pty to the poller failed");
        return Repark::Keep(file, tx);
    }
    slot.join.take();
    slot.state = SlotState::Parked(ParkedPty { file, tx });
    drop(slots);
    park.poke();
    Repark::Parked
}

enum PollRead {
    Timeout,
    Ready,
    Hungup,
}

fn poll_readable(file: &File, wait: Duration) -> PollRead {
    let millis = u16::try_from(wait.as_millis()).unwrap_or(u16::MAX);
    let mut fds = [nix::poll::PollFd::new(
        file.as_fd(),
        nix::poll::PollFlags::POLLIN,
    )];
    match nix::poll::poll(&mut fds, nix::poll::PollTimeout::from(millis)) {
        Ok(0) | Err(nix::errno::Errno::EINTR) => PollRead::Timeout,
        Ok(_) => match fds[0].revents() {
            Some(flags)
                if flags.contains(nix::poll::PollFlags::POLLHUP)
                    && !flags.contains(nix::poll::PollFlags::POLLIN) =>
            {
                PollRead::Hungup
            }
            Some(flags)
                if flags.intersects(
                    nix::poll::PollFlags::POLLIN
                        | nix::poll::PollFlags::POLLHUP
                        | nix::poll::PollFlags::POLLERR
                        | nix::poll::PollFlags::POLLNVAL,
                ) =>
            {
                PollRead::Ready
            }
            _ => PollRead::Timeout,
        },
        Err(err) => {
            tracing::debug!(?err, "pty reader poll failed");
            PollRead::Hungup
        }
    }
}

fn spawn_dedicated(file: File, tx: mpsc::Sender<PtyEvent>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("phux-pty-reader".to_owned())
        .stack_size(READER_STACK)
        .spawn(move || {
            let mut file = file;
            dedicated_loop(&mut file, &tx);
        })
        .unwrap_or_else(|err| {
            tracing::error!(?err, "pty reader thread failed to start");
            // A joined dummy keeps `PtyReader`'s shutdown path uniform.
            std::thread::spawn(|| {})
        })
}

fn dedicated_loop(file: &mut File, tx: &mpsc::Sender<PtyEvent>) {
    crate::perf::promote_helper_thread("phux-pty-reader");
    let mut buf = vec![0_u8; PTY_READ_CHUNK];
    loop {
        match file.read(&mut buf) {
            Ok(0) => {
                let _ = tx.blocking_send(PtyEvent::Eof);
                break;
            }
            Ok(n) => {
                note_read(n);
                let chunk = bytes::Bytes::copy_from_slice(&buf[..n]);
                if send_pty_chunk(tx, chunk, Instant::now()).is_break() {
                    drain_master_to_eof(file, &mut buf);
                    break;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) if err.raw_os_error() == Some(super::spawn::EIO) => {
                let _ = tx.blocking_send(PtyEvent::Eof);
                break;
            }
            Err(err) => {
                tracing::debug!(?err, "pty reader thread: read error");
                let _ = tx.blocking_send(PtyEvent::Eof);
                break;
            }
        }
    }
}

fn set_cloexec_nonblock(fd: &OwnedFd) -> io::Result<()> {
    nix::fcntl::fcntl(
        fd,
        nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
    )
    .map_err(io::Error::from)?;
    let flags = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFL).map_err(io::Error::from)?;
    let mut flags = nix::fcntl::OFlag::from_bits_retain(flags);
    flags.insert(nix::fcntl::OFlag::O_NONBLOCK);
    nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFL(flags)).map_err(io::Error::from)?;
    Ok(())
}

fn drain_fd(fd: &OwnedFd) {
    let mut buf = [0_u8; 64];
    loop {
        match nix::unistd::read(fd, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

enum Reactor {
    #[cfg(target_os = "linux")]
    Epoll(nix::sys::epoll::Epoll),
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))]
    Kqueue(nix::sys::event::Kqueue),
}

impl Reactor {
    fn start() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let epoll =
                nix::sys::epoll::Epoll::new(nix::sys::epoll::EpollCreateFlags::EPOLL_CLOEXEC)
                    .map_err(io::Error::from)?;
            return Ok(Self::Epoll(epoll));
        }
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "dragonfly"
        ))]
        {
            let kqueue = nix::sys::event::Kqueue::new().map_err(io::Error::from)?;
            return Ok(Self::Kqueue(kqueue));
        }
        #[allow(unreachable_code)]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no pty poller for this platform",
        ))
    }

    fn add(&self, fd: std::os::fd::BorrowedFd<'_>, id: u64) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            let Self::Epoll(epoll) = self;
            let event = nix::sys::epoll::EpollEvent::new(nix::sys::epoll::EpollFlags::EPOLLIN, id);
            return epoll.add(fd, event).map_err(io::Error::from);
        }
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "dragonfly"
        ))]
        {
            let Self::Kqueue(kqueue) = self;
            let change = kqueue_change(fd, nix::sys::event::EvFlags::EV_ADD, id)?;
            return kqueue
                .kevent(std::slice::from_ref(&change), &mut [], None)
                .map(|_| ())
                .map_err(io::Error::from);
        }
        #[allow(unreachable_code)]
        {
            let _ = (fd, id);
            Err(io::Error::other("no pty poller"))
        }
    }

    fn delete(&self, fd: std::os::fd::BorrowedFd<'_>) {
        #[cfg(target_os = "linux")]
        {
            let Self::Epoll(epoll) = self;
            let _ = epoll.delete(fd);
            return;
        }
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "dragonfly"
        ))]
        {
            let Self::Kqueue(kqueue) = self;
            let Ok(change) = kqueue_change(fd, nix::sys::event::EvFlags::EV_DELETE, 0) else {
                return;
            };
            let _ = kqueue.kevent(std::slice::from_ref(&change), &mut [], None);
        }
    }

    fn wait(&self) -> io::Result<Vec<u64>> {
        #[cfg(target_os = "linux")]
        {
            let Self::Epoll(epoll) = self;
            let mut events = [nix::sys::epoll::EpollEvent::empty(); 64];
            let n = epoll
                .wait(&mut events, nix::sys::epoll::EpollTimeout::NONE)
                .map_err(io::Error::from)?;
            return Ok(events
                .iter()
                .take(n)
                .map(nix::sys::epoll::EpollEvent::data)
                .collect());
        }
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "dragonfly"
        ))]
        {
            let Self::Kqueue(kqueue) = self;
            let mut events = vec![
                nix::sys::event::KEvent::new(
                    0,
                    nix::sys::event::EventFilter::EVFILT_READ,
                    nix::sys::event::EvFlags::empty(),
                    nix::sys::event::FilterFlag::empty(),
                    0,
                    0,
                );
                64
            ];
            let n = kqueue
                .kevent(&[], &mut events, None)
                .map_err(io::Error::from)?;
            return Ok(events
                .iter()
                .take(n)
                .filter(|event| !event.flags().contains(nix::sys::event::EvFlags::EV_ERROR))
                .filter_map(|event| u64::try_from(event.udata()).ok())
                .collect());
        }
        #[allow(unreachable_code)]
        Err(io::Error::other("no pty poller"))
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
fn kqueue_change(
    fd: std::os::fd::BorrowedFd<'_>,
    flags: nix::sys::event::EvFlags,
    id: u64,
) -> io::Result<nix::sys::event::KEvent> {
    let ident = usize::try_from(fd.as_raw_fd())
        .map_err(|_| io::Error::other("pty fd does not fit kevent ident"))?;
    let udata =
        isize::try_from(id).map_err(|_| io::Error::other("pty id does not fit kevent udata"))?;
    Ok(nix::sys::event::KEvent::new(
        ident,
        nix::sys::event::EventFilter::EVFILT_READ,
        flags,
        nix::sys::event::FilterFlag::empty(),
        0,
        udata,
    ))
}

/// Spawn the poller thread once the park is stored in the process-lifetime slot.
fn boot_poller(park: &'static Park) {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| park.boot());
}
