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

use super::spawn::{PTY_READ_BATCH, PTY_READ_CHUNK, PtyEvent, drain_master_to_eof, send_pty_chunk};

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

/// Scratch plus the batch reused across wakes. A full handoff moves the
/// allocation to the actor; a short one leaves the spare capacity here.
struct CoalesceBufs {
    scratch: Vec<u8>,
    batch: BytesMut,
}

impl CoalesceBufs {
    fn new() -> Self {
        Self {
            scratch: vec![0_u8; PTY_READ_CHUNK],
            batch: BytesMut::with_capacity(PTY_READ_BATCH),
        }
    }
}

thread_local! {
    static POLLER_BUFFERS: RefCell<CoalesceBufs> = RefCell::new(CoalesceBufs::new());
}

/// What one wake collected. Bytes, when present, sit in the caller's batch.
#[derive(Debug, PartialEq, Eq)]
enum Gathered {
    /// The first read would block. Nothing was consumed.
    Pending,
    /// The master closed with nothing left to deliver.
    Eof,
    /// The batch holds this wake's bytes, in read order.
    Bytes {
        /// The master hit EOF after those bytes.
        eof: bool,
    },
}

/// One nonblocking `read(2)`: `WouldBlock` when nothing is waiting. Tests
/// supply a script.
trait PtySource {
    fn pull(&mut self, dst: &mut [u8]) -> io::Result<usize>;
}

struct MasterFile<'a> {
    file: &'a mut File,
}

impl PtySource for MasterFile<'_> {
    fn pull(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        let n = read_available(self.file, dst)?;
        if n > 0 {
            note_read(n);
        }
        Ok(n)
    }
}

enum Pull {
    Data,
    Blocked,
    Ended,
}

fn prepare_batch(batch: &mut BytesMut) {
    batch.clear();
    // Spare below one kernel read is the tail of an allocation the actor
    // still holds. Start a fresh batch instead of trickling that tail out.
    if batch.capacity() < PTY_READ_CHUNK {
        batch.reserve(PTY_READ_BATCH);
    }
}

fn readable_room(batch: &BytesMut) -> usize {
    let until_cap = PTY_READ_BATCH.saturating_sub(batch.len());
    let spare = batch.capacity().saturating_sub(batch.len());
    until_cap.min(spare)
}

fn pull_some<S: PtySource>(source: &mut S, scratch: &mut [u8], batch: &mut BytesMut) -> Pull {
    let room = readable_room(batch);
    if room == 0 {
        return Pull::Blocked;
    }
    let want = room.min(scratch.len());
    loop {
        match source.pull(&mut scratch[..want]) {
            Ok(0) => return Pull::Ended,
            Ok(n) => {
                batch.extend_from_slice(&scratch[..n]);
                return Pull::Data;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Pull::Blocked,
            Err(err) if err.raw_os_error() == Some(super::spawn::EIO) => return Pull::Ended,
            Err(err) => {
                tracing::debug!(?err, "pty read failed");
                return Pull::Ended;
            }
        }
    }
}

/// Drain until the next read would block or the batch cap is full.
///
/// The master is `O_NONBLOCK`, so a read with nothing waiting returns
/// `EAGAIN` at once: one short write is handed off with no timer, and a
/// burst costs one syscall per read rather than a `poll(2)` before each.
/// A hangup surfaces as EOF or `EIO` from the read itself.
fn gather_wake<S: PtySource>(source: &mut S, scratch: &mut [u8], batch: &mut BytesMut) -> Gathered {
    prepare_batch(batch);
    match pull_some(source, scratch, batch) {
        Pull::Blocked => return Gathered::Pending,
        Pull::Ended => return Gathered::Eof,
        Pull::Data => {}
    }
    loop {
        match pull_some(source, scratch, batch) {
            Pull::Data => {}
            Pull::Blocked => return Gathered::Bytes { eof: false },
            Pull::Ended => return Gathered::Bytes { eof: true },
        }
    }
}

fn read_once(file: &mut File, tx: &mpsc::Sender<PtyEvent>) -> ReadStep {
    POLLER_BUFFERS.with(|bufs| {
        let bufs = &mut *bufs.borrow_mut();
        let gathered = gather_wake(&mut MasterFile { file }, &mut bufs.scratch, &mut bufs.batch);
        match gathered {
            Gathered::Pending => ReadStep::Retry,
            Gathered::Eof => deliver_terminal(tx, PtyEvent::Eof),
            Gathered::Bytes { eof } => publish_batch(tx, &mut bufs.batch, eof),
        }
    })
}

fn publish_batch(tx: &mpsc::Sender<PtyEvent>, batch: &mut BytesMut, eof: bool) -> ReadStep {
    let event = PtyEvent::Bytes {
        chunk: batch.split().freeze(),
        read_at: Instant::now(),
    };
    match tx.try_send(event) {
        Ok(()) => {
            if eof {
                deliver_terminal(tx, PtyEvent::Eof)
            } else {
                ReadStep::Promote(Vec::new())
            }
        }
        Err(TrySendError::Full(event)) => {
            let mut pending = vec![event];
            if eof {
                pending.push(PtyEvent::Eof);
            }
            ReadStep::Promote(pending)
        }
        Err(TrySendError::Closed(_)) => ReadStep::Closed,
    }
}

fn deliver_terminal(tx: &mpsc::Sender<PtyEvent>, event: PtyEvent) -> ReadStep {
    match tx.try_send(event) {
        Ok(()) => ReadStep::Eof,
        Err(TrySendError::Full(event)) => ReadStep::Promote(vec![event]),
        Err(TrySendError::Closed(_)) => ReadStep::Closed,
    }
}

/// Drain `file` until EOF or the orphan budget. The master is nonblocking,
/// so an empty read waits in `poll(2)` the way a blocking `read(2)` would
/// instead of sleeping between reads: a child flushing a large write
/// through a small PTY buffer keeps its full throughput.
fn drain_file_to_eof(file: &mut File, buf: &mut [u8]) {
    drain_master_to_eof(&mut ReadyRead(file), buf);
}

/// A read on the nonblocking master that waits for data, as a blocking read
/// would. A hangup with nothing left to read is EOF.
struct ReadyRead<'a>(&'a mut File);

impl Read for ReadyRead<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.0.read(buf) {
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    if matches!(poll_readable(self.0, CANCEL_SLICE), PollRead::Hungup) {
                        return Ok(0);
                    }
                }
                other => return other,
            }
        }
    }
}

fn detach_drain(mut file: File) {
    let spawned = std::thread::Builder::new()
        .name("phux-pty-drain".to_owned())
        .stack_size(READER_STACK)
        .spawn(move || {
            let mut buf = vec![0_u8; PTY_READ_CHUNK];
            drain_file_to_eof(&mut file, &mut buf);
        });
    if let Err(err) = spawned {
        tracing::warn!(?err, "pty orphan drain thread failed to start");
    }
}

pub(super) fn note_read(n: usize) {
    crate::perf::PTY_READ_SIZE.record_len(n);
    crate::perf::PTY_READ_BYTES.add_len(n);
    phux_perf::echo_hop::record("pty_read");
    tracing::debug!(n, "pty read");
}

/// One read. The master is `O_NONBLOCK`, so a direct echo steal that already
/// consumed the byte returns [`ErrorKind::WouldBlock`] instead of parking
/// the poller in `read(2)`.
fn read_available(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    file.read(buf)
}

/// Mark `file`'s open file description nonblocking. Dups of that description
/// (the writer, the echo steal) share the flag.
pub(super) fn set_nonblock(file: &File) -> io::Result<()> {
    let flags =
        nix::fcntl::fcntl(file.as_fd(), nix::fcntl::FcntlArg::F_GETFL).map_err(io::Error::from)?;
    let mut flags = nix::fcntl::OFlag::from_bits_retain(flags);
    flags.insert(nix::fcntl::OFlag::O_NONBLOCK);
    nix::fcntl::fcntl(file.as_fd(), nix::fcntl::FcntlArg::F_SETFL(flags))
        .map_err(io::Error::from)?;
    Ok(())
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
    let mut bufs = CoalesceBufs::new();
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
                if handle_hot_read(park, id, &mut file, &tx, &mut bufs, &mut last).is_exit() {
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
    bufs: &mut CoalesceBufs,
    last: &mut Instant,
) -> AfterRead {
    match gather_wake(&mut MasterFile { file }, &mut bufs.scratch, &mut bufs.batch) {
        Gathered::Pending => AfterRead::Stay,
        Gathered::Eof => finish_hot(park, id, tx, PtyEvent::Eof),
        Gathered::Bytes { eof } => {
            *last = Instant::now();
            let chunk = bufs.batch.split().freeze();
            if send_pty_chunk(tx, chunk, *last).is_break() {
                drain_file_to_eof(file, &mut bufs.scratch);
                park.remove(id);
                return AfterRead::Exit;
            }
            if eof {
                finish_hot(park, id, tx, PtyEvent::Eof)
            } else {
                AfterRead::Stay
            }
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
                    drain_file_to_eof(file, &mut buf);
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
    let mut bufs = CoalesceBufs::new();
    loop {
        match gather_wake(&mut MasterFile { file }, &mut bufs.scratch, &mut bufs.batch) {
            // The master is `O_NONBLOCK`. A direct echo steal already took
            // the byte, so wait for the next wake instead of spinning.
            Gathered::Pending => {
                if matches!(poll_readable(file, CANCEL_SLICE), PollRead::Hungup) {
                    let _ = tx.blocking_send(PtyEvent::Eof);
                    break;
                }
            }
            Gathered::Eof => {
                let _ = tx.blocking_send(PtyEvent::Eof);
                break;
            }
            Gathered::Bytes { eof } => {
                let chunk = bufs.batch.split().freeze();
                if send_pty_chunk(tx, chunk, Instant::now()).is_break() {
                    drain_file_to_eof(file, &mut bufs.scratch);
                    break;
                }
                if eof {
                    let _ = tx.blocking_send(PtyEvent::Eof);
                    break;
                }
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
    use std::os::fd::AsRawFd;
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

#[cfg(test)]
mod coalesce_tests {
    use std::collections::VecDeque;
    use std::fs::File;
    use std::io::Write;
    use std::time::{Duration, Instant};

    use bytes::BytesMut;

    use super::{Gathered, MasterFile, PTY_READ_BATCH, PTY_READ_CHUNK, PtySource, gather_wake};

    fn pattern_byte(index: usize) -> u8 {
        u8::try_from(index % 251).expect("modulo 251 fits in a byte")
    }

    /// Run `work` on another thread so a blocked read fails the test instead
    /// of hanging the suite.
    fn within<T: Send + 'static>(budget: Duration, work: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
        rx.recv_timeout(budget).unwrap_or_else(|_| {
            panic!("read was not handed off within {budget:?}");
        })
    }

    enum Step {
        Data(Vec<u8>),
        Eof,
        Interrupted,
        Error(std::io::ErrorKind),
    }

    struct Script {
        steps: VecDeque<Step>,
        pulls: usize,
    }

    impl PtySource for Script {
        fn pull(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
            self.pulls += 1;
            match self.steps.pop_front() {
                Some(Step::Data(data)) => {
                    let n = data.len().min(dst.len());
                    dst[..n].copy_from_slice(&data[..n]);
                    if n < data.len() {
                        self.steps.push_front(Step::Data(data[n..].to_vec()));
                    }
                    Ok(n)
                }
                Some(Step::Eof) => Ok(0),
                Some(Step::Interrupted) => {
                    Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
                }
                Some(Step::Error(kind)) => Err(std::io::Error::new(kind, "scripted read")),
                None => Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
            }
        }
    }

    struct Pattern {
        next: usize,
        end: usize,
        chunk: usize,
    }

    /// A writer that stays open: once drained, reads would block.
    impl PtySource for Pattern {
        fn pull(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
            if self.next >= self.end {
                return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
            }
            let n = dst.len().min(self.chunk).min(self.end - self.next);
            for (offset, byte) in dst[..n].iter_mut().enumerate() {
                *byte = pattern_byte(self.next + offset);
            }
            self.next += n;
            Ok(n)
        }
    }

    fn gather(source: &mut impl PtySource, batch: &mut BytesMut) -> Gathered {
        let mut scratch = vec![0_u8; PTY_READ_CHUNK];
        gather_wake(source, &mut scratch, batch)
    }

    #[test]
    fn short_chunks_keep_their_order_across_a_would_block() {
        let mut script = Script {
            steps: VecDeque::from([
                Step::Data(b"one".to_vec()),
                Step::Data(b"-two".to_vec()),
                Step::Data(b"-three".to_vec()),
            ]),
            pulls: 0,
        };
        let mut batch = BytesMut::with_capacity(PTY_READ_BATCH);
        let gathered = gather(&mut script, &mut batch);
        assert_eq!(gathered, Gathered::Bytes { eof: false });
        assert_eq!(&batch[..], b"one-two-three");
        assert_eq!(
            script.pulls, 4,
            "the wake must stop at the first read that would block"
        );

        script.steps.push_back(Step::Data(b"-four".to_vec()));
        let gathered = gather(&mut script, &mut batch);
        assert_eq!(gathered, Gathered::Bytes { eof: false });
        assert_eq!(&batch[..], b"-four");
    }

    #[test]
    fn a_single_small_read_is_handed_off_without_waiting() {
        let (gathered, bytes, pulls, elapsed) = within(Duration::from_secs(1), || {
            let mut script = Script {
                steps: VecDeque::from([Step::Data(b"Z".to_vec())]),
                pulls: 0,
            };
            let mut batch = BytesMut::with_capacity(PTY_READ_BATCH);
            let started = Instant::now();
            let gathered = gather(&mut script, &mut batch);
            (
                gathered,
                Vec::from(&batch[..]),
                script.pulls,
                started.elapsed(),
            )
        });
        assert_eq!(gathered, Gathered::Bytes { eof: false });
        assert_eq!(bytes, b"Z");
        assert_eq!(pulls, 2, "the second read finds nothing and ends the wake");
        assert!(
            elapsed < Duration::from_millis(200),
            "a single byte must not sit on a coalesce timer, waited {elapsed:?}"
        );
    }

    #[test]
    fn the_cap_is_respected_and_the_remainder_stays_in_order() {
        let total = PTY_READ_BATCH * 2 + 31;
        let mut pattern = Pattern {
            next: 0,
            end: total,
            chunk: 10_000,
        };
        let mut batch = BytesMut::new();
        let mut got = Vec::with_capacity(total);
        let mut lengths = Vec::new();
        loop {
            match gather(&mut pattern, &mut batch) {
                Gathered::Bytes { eof: false } => {
                    assert!(
                        batch.len() <= PTY_READ_BATCH,
                        "batch of {} exceeds the cap",
                        batch.len()
                    );
                    assert!(!batch.is_empty());
                    lengths.push(batch.len());
                    got.extend_from_slice(&batch);
                    let _ = batch.split();
                }
                Gathered::Pending => break,
                other => panic!("unexpected {other:?} after {} bytes", got.len()),
            }
        }
        assert_eq!(
            lengths[0], PTY_READ_BATCH,
            "queued data fills the cap exactly"
        );
        assert_eq!(lengths[1], PTY_READ_BATCH);
        assert_eq!(lengths[2], 31);
        assert_eq!(got.len(), total);
        assert!(
            got.iter()
                .enumerate()
                .all(|(index, byte)| *byte == pattern_byte(index)),
            "bytes were lost or reordered"
        );
    }

    #[test]
    fn eof_and_errors_keep_the_bytes_already_read() {
        let mut eof_after = Script {
            steps: VecDeque::from([Step::Data(b"tail".to_vec()), Step::Eof]),
            pulls: 0,
        };
        let mut batch = BytesMut::new();
        assert_eq!(
            gather(&mut eof_after, &mut batch),
            Gathered::Bytes { eof: true }
        );
        assert_eq!(&batch[..], b"tail");

        let mut bare_eof = Script {
            steps: VecDeque::from([Step::Eof]),
            pulls: 0,
        };
        assert_eq!(gather(&mut bare_eof, &mut batch), Gathered::Eof);
        assert!(batch.is_empty());

        let mut broken = Script {
            steps: VecDeque::from([
                Step::Interrupted,
                Step::Data(b"xy".to_vec()),
                Step::Error(std::io::ErrorKind::BrokenPipe),
            ]),
            pulls: 0,
        };
        assert_eq!(
            gather(&mut broken, &mut batch),
            Gathered::Bytes { eof: true }
        );
        assert_eq!(&batch[..], b"xy");
    }

    /// A pipe whose read end is nonblocking, as the PTY master is.
    fn pipe_pair() -> (File, File) {
        let (read, write) = nix::unistd::pipe().expect("pipe");
        let read = File::from(read);
        super::set_nonblock(&read).expect("nonblocking read end");
        (read, File::from(write))
    }

    #[test]
    fn a_single_pipe_byte_is_handed_off_without_blocking_for_more() {
        let (gathered, bytes, elapsed) = within(Duration::from_secs(2), || {
            let (mut read, mut write) = pipe_pair();
            write.write_all(b"Z").expect("write one byte");
            let mut batch = BytesMut::with_capacity(PTY_READ_BATCH);
            let started = Instant::now();
            let gathered = gather(&mut MasterFile { file: &mut read }, &mut batch);
            (gathered, Vec::from(&batch[..]), started.elapsed())
        });
        assert_eq!(gathered, Gathered::Bytes { eof: false });
        assert_eq!(bytes, b"Z");
        assert!(
            elapsed < Duration::from_millis(200),
            "the follow-up read must not wait, waited {elapsed:?}"
        );
    }

    #[test]
    fn closing_the_writer_delivers_the_tail_and_then_eof() {
        let (mut read, mut write) = pipe_pair();
        write.write_all(b"abcdef").expect("write");
        drop(write);
        let mut batch = BytesMut::new();
        let (gathered, bytes) = within(Duration::from_secs(2), move || {
            let gathered = gather(&mut MasterFile { file: &mut read }, &mut batch);
            (gathered, Vec::from(&batch[..]))
        });
        assert_eq!(gathered, Gathered::Bytes { eof: true });
        assert_eq!(bytes, b"abcdef");
    }

    /// The kernel pipe buffer has to hold more than one batch, which
    /// `F_SETPIPE_SZ` provides on Linux.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_prefilled_pipe_stops_at_the_cap_and_continues_in_order() {
        let total = PTY_READ_BATCH * 2 + 31;
        let payload: Vec<u8> = (0..total).map(pattern_byte).collect();
        let (first, lengths, got) = within(Duration::from_secs(2), move || {
            let (read, mut write) = pipe_pair();
            nix::fcntl::fcntl(&read, nix::fcntl::FcntlArg::F_SETPIPE_SZ(256 * 1024))
                .expect("enlarge the pipe");
            write.write_all(&payload).expect("prefill the pipe");
            let mut read = read;
            let mut batch = BytesMut::new();
            let first = gather(&mut MasterFile { file: &mut read }, &mut batch);
            let mut got = Vec::from(&batch[..]);
            let mut lengths = vec![got.len()];
            let _ = batch.split();
            while got.len() < total {
                match gather(&mut MasterFile { file: &mut read }, &mut batch) {
                    Gathered::Bytes { eof: false } => {
                        lengths.push(batch.len());
                        got.extend_from_slice(&batch);
                        let _ = batch.split();
                    }
                    other => panic!("unexpected {other:?} after {} bytes", got.len()),
                }
            }
            (first, lengths, got)
        });
        assert_eq!(first, Gathered::Bytes { eof: false });
        assert_eq!(lengths, vec![PTY_READ_BATCH, PTY_READ_BATCH, 31]);
        assert!(
            got.iter()
                .enumerate()
                .all(|(index, byte)| *byte == pattern_byte(index))
        );
    }
}
