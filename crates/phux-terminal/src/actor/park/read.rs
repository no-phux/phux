//! Reading a parked PTY: batching, coalescing, publication.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

pub(in crate::actor) struct ReadJob {
    pub(in crate::actor) file: File,
    pub(in crate::actor) tx: mpsc::Sender<PtyEvent>,
    pub(in crate::actor) cancel: std::sync::Arc<AtomicBool>,
}

pub(in crate::actor) enum ReadStep {
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
pub(in crate::actor) struct CoalesceBufs {
    pub(in crate::actor) scratch: Vec<u8>,
    pub(in crate::actor) batch: BytesMut,
}

impl CoalesceBufs {
    pub(in crate::actor) fn new() -> Self {
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
pub(in crate::actor) enum Gathered {
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
pub(in crate::actor) trait PtySource {
    fn pull(&mut self, dst: &mut [u8]) -> io::Result<usize>;
}

pub(in crate::actor) struct MasterFile<'a> {
    pub(in crate::actor) file: &'a mut File,
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

pub(in crate::actor) enum Pull {
    Data,
    Blocked,
    Ended,
}

pub(in crate::actor) fn prepare_batch(batch: &mut BytesMut) {
    batch.clear();
    // Spare below one kernel read is the tail of an allocation the actor
    // still holds. Start a fresh batch instead of trickling that tail out.
    if batch.capacity() < PTY_READ_CHUNK {
        batch.reserve(PTY_READ_BATCH);
    }
}

pub(in crate::actor) fn readable_room(batch: &BytesMut) -> usize {
    let until_cap = PTY_READ_BATCH.saturating_sub(batch.len());
    let spare = batch.capacity().saturating_sub(batch.len());
    until_cap.min(spare)
}

pub(in crate::actor) fn pull_some<S: PtySource>(
    source: &mut S,
    scratch: &mut [u8],
    batch: &mut BytesMut,
) -> Pull {
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
            Err(err) if err.raw_os_error() == Some(super::super::spawn::EIO) => return Pull::Ended,
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
pub(in crate::actor) fn gather_wake<S: PtySource>(
    source: &mut S,
    scratch: &mut [u8],
    batch: &mut BytesMut,
) -> Gathered {
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

pub(in crate::actor) fn read_once(file: &mut File, tx: &mpsc::Sender<PtyEvent>) -> ReadStep {
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

pub(in crate::actor) fn publish_batch(
    tx: &mpsc::Sender<PtyEvent>,
    batch: &mut BytesMut,
    eof: bool,
) -> ReadStep {
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

pub(in crate::actor) fn deliver_terminal(tx: &mpsc::Sender<PtyEvent>, event: PtyEvent) -> ReadStep {
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
pub(in crate::actor) fn drain_file_to_eof(file: &mut File, buf: &mut [u8]) {
    drain_master_to_eof(&mut ReadyRead(file), buf);
}

/// A read on the nonblocking master that waits for data, as a blocking read
/// would. A hangup with nothing left to read is EOF.
pub(in crate::actor) struct ReadyRead<'a>(&'a mut File);

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

pub(in crate::actor) fn detach_drain(mut file: File) {
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

pub(in crate::actor) fn note_read(n: usize) {
    phux_resource::perf::PTY_READ_SIZE.record_len(n);
    phux_resource::perf::PTY_READ_BYTES.add_len(n);
    phux_perf::echo_hop::record("pty_read");
    tracing::debug!(n, "pty read");
}

/// One nonblocking read: an empty master returns [`ErrorKind::WouldBlock`]
/// instead of parking the poller in `read(2)`.
pub(in crate::actor) fn read_available(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    file.read(buf)
}

/// Mark `file`'s open file description nonblocking. Dups of that description
/// (including the writer) share the flag.
pub(in crate::actor) fn set_nonblock(file: &File) -> io::Result<()> {
    let flags =
        nix::fcntl::fcntl(file.as_fd(), nix::fcntl::FcntlArg::F_GETFL).map_err(io::Error::from)?;
    let mut flags = nix::fcntl::OFlag::from_bits_retain(flags);
    flags.insert(nix::fcntl::OFlag::O_NONBLOCK);
    nix::fcntl::fcntl(file.as_fd(), nix::fcntl::FcntlArg::F_SETFL(flags))
        .map_err(io::Error::from)?;
    Ok(())
}
