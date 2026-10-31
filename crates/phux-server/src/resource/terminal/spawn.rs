//! PTY spawn/adopt, the reader and writer bridge threads, and pane command
//! construction.

use super::{EncodedInputRequest, TerminalActorError};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tokio::sync::mpsc;
use tracing::debug;

mod command;
mod exit;
mod write;

pub use command::*;
pub(crate) use exit::*;
pub(super) use write::*;

/// One `read(2)` into the coalescing buffer. macOS returns at most 1024
/// bytes per read and a Linux PTY master at most 4095, so this takes a full
/// kernel chunk in one syscall.
pub(super) const PTY_READ_CHUNK: usize = 16 * 1024;

/// Bytes one wake may hand the actor.
///
/// Later reads in that wake do not block, so a short interactive read is
/// sent at once and nothing waits on a timer. The cap only bounds a flood
/// that already has data queued. `PTY_READ_BATCH * PTY_CHANNEL_DEPTH` is the
/// per-pane queue bound.
pub(super) const PTY_READ_BATCH: usize = 64 * 1024;

/// Depth of the reader-thread -> actor PTY channel.
///
/// Bounded so a runaway producer gets backpressure (the reader blocks, the
/// child stalls in the line discipline) instead of unbounded memory. The
/// actor never waits on the reader, so blocking it cannot deadlock. The
/// depth must exceed `MAX_PTY_COALESCE` or it throttles the batcher.
pub(super) const PTY_CHANNEL_DEPTH: usize = 128;

/// PTY-side resources of a [`TerminalActor`](crate::terminal_actor::TerminalActor).
///
/// Drop order is the reverse of declaration: writer thread (VEOF), reader
/// dup, child, then the master. Dropping the reader hangs up a master the
/// poller was still holding.
pub(crate) struct PtyOwned {
    /// Master handle, kept for resize ioctls and the writer's termios checks.
    pub(crate) master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// Child on the slave side, reaped in `TerminalActor::shutdown_pty`.
    pub(crate) child: Box<dyn Child + Send + Sync>,
    /// Quiet-poller registration, or a dedicated reader when the pane is hot.
    pub(super) reader: super::park::PtyReader,
    /// Writer thread; exits when its receiver closes.
    pub(crate) writer_thread: Option<JoinHandle<()>>,
    /// Inline-write gate shared with the receive task.
    pub(super) fast: Arc<super::fast_pty::PtyFastPath>,
}

impl std::fmt::Debug for PtyOwned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyOwned")
            .field("child", &self.child)
            .finish_non_exhaustive()
    }
}

/// Events flowing from the PTY reader thread into the actor.
#[derive(Debug)]
pub(crate) enum PtyEvent {
    /// A chunk read from the master, refcounted for broadcast. `read_at`
    /// feeds the `pty.queue_wait` metric.
    Bytes {
        /// The bytes read.
        chunk: bytes::Bytes,
        /// When the read completed.
        read_at: std::time::Instant,
    },
    /// The PTY hit EOF or errored. Either way: the child is going away.
    Eof,
}

type SpawnedPty = (
    mpsc::Receiver<PtyEvent>,
    mpsc::Sender<EncodedInputRequest>,
    PtyOwned,
);

/// Receive from `rx` when `Some`; otherwise pend forever (a select! arm for
/// PTY-less actors).
pub(crate) async fn recv_or_pending(rx: Option<&mut mpsc::Receiver<PtyEvent>>) -> Option<PtyEvent> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Open a PTY, spawn `cmd` on the slave, and start the bridge threads.
pub(crate) fn spawn_pty(
    mut cmd: CommandBuilder,
    cols: u16,
    rows: u16,
) -> Result<SpawnedPty, TerminalActorError> {
    let pty_system = native_pty_system();
    // Nonzero initial pixel size for children that query `TIOCGWINSZ` before
    // the first client resize.
    let (cell_w, cell_h) = super::DEFAULT_CELL_PX;
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: cols.saturating_mul(cell_w),
            pixel_height: rows.saturating_mul(cell_h),
        })
        .map_err(|e| TerminalActorError::OpenPty(e.to_string()))?;

    clear_upgrade_handoff_env(&mut cmd);
    clear_agent_host_env(&mut cmd);
    // TERM retains the ncurses-compatible terminfo choice. Truecolor is a
    // separate capability, independent of how the daemon was launched.
    cmd.env("COLORTERM", "truecolor");
    let child = super::fd_shrink::spawn_pane_child(&*pair.slave, &*pair.master, &cmd)
        .map_err(TerminalActorError::Launch)?;
    // Our slave copy would prevent EOF on the master after the child exits.
    drop(pair.slave);

    start_pty_bridge(pair.master, child)
}

/// A PTY spawn failure as one line a user can act on. portable-pty's
/// command-not-found text spans lines and quotes the whole `PATH` it
/// searched, which the refusal would carry verbatim to every client.
#[cfg(test)]
fn spawn_failure_reason(err: &dyn std::fmt::Display) -> String {
    const NOT_FOUND: &str = "No viable candidates found in PATH";
    let text = err.to_string();
    let text = match text.find(NOT_FOUND) {
        Some(at) => format!("{}not found in PATH", &text[..at]),
        None => text,
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Adopt an inherited PTY master fd and child pid after a graceful-upgrade
/// exec (ADR-0032) and start fresh bridge threads around them.
pub(crate) fn adopt_pty(
    master_fd: std::os::fd::RawFd,
    child_pid: i32,
) -> Result<SpawnedPty, TerminalActorError> {
    // SAFETY: `master_fd` is the inherited PTY master (FD_CLOEXEC cleared
    // before the exec), owned solely by this process now.
    let master: Box<dyn MasterPty + Send> =
        Box::new(unsafe { portable_pty_adopt::AdoptedMaster::from_raw_fd(master_fd) });
    let child: Box<dyn Child + Send + Sync> =
        Box::new(portable_pty_adopt::AdoptedChild::new(child_pid));
    start_pty_bridge(master, child)
}

/// Budget for the reader's post-actor drain: the hangup grace plus the reap
/// budget, i.e. the whole window in which the child may still be writing.
/// It only bounds a child that outlives the actor and writes forever.
const ORPHAN_DRAIN_BUDGET: std::time::Duration = {
    // Tests stretch the hangup ceiling (up to a 30 s trap-marker hold plus
    // 2.5 s); the reader cannot see that gate, so include it. EOF still ends
    // the drain.
    const GRACE: std::time::Duration = if cfg!(test) {
        std::time::Duration::from_millis(30_000 + 2_500)
    } else {
        super::PANE_KILL_GRACE
    };
    GRACE.saturating_add(super::PANE_KILL_REAP_BUDGET)
};

/// Keep draining the master after the actor is gone, until EOF or
/// [`ORPHAN_DRAIN_BUDGET`]. An unread master blocks the child's `write(2)`
/// after ~1 KiB, which would wedge a job flushing in its `SIGHUP` handler.
///
/// The budget is checked only between reads; a read blocked by an outside
/// holder of the slave is why `shutdown_pty` bounds the join itself.
pub(super) fn drain_master_to_eof<R: Read>(reader: &mut R, buf: &mut [u8]) {
    drain_master_with_budget(reader, buf, ORPHAN_DRAIN_BUDGET);
}

/// [`drain_master_to_eof`] with an explicit budget (for tests).
fn drain_master_with_budget<R: Read>(reader: &mut R, buf: &mut [u8], budget: std::time::Duration) {
    let deadline = std::time::Instant::now() + budget;
    loop {
        match reader.read(buf) {
            Ok(0) => return,
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                // The poller's master is nonblocking. An orphan drain still
                // has to wait for the child.
                if std::time::Instant::now() >= deadline {
                    debug!(
                        "pty reader thread: orphan drain budget expired; leaving the master unread"
                    );
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(err) => {
                debug!(?err, "pty reader thread: read error while draining orphan");
                return;
            }
        }
        if std::time::Instant::now() >= deadline {
            debug!("pty reader thread: orphan drain budget expired; leaving the master unread");
            return;
        }
    }
}

/// Hand one PTY chunk to the actor: `try_send` first (cheap), then
/// `blocking_send` only when the queue is really full, which is the
/// backpressure that stalls a runaway child. `Break` means the actor is gone.
pub(super) fn send_pty_chunk(
    tx: &mpsc::Sender<PtyEvent>,
    chunk: bytes::Bytes,
    read_at: std::time::Instant,
) -> std::ops::ControlFlow<()> {
    use tokio::sync::mpsc::error::TrySendError;
    match tx.try_send(PtyEvent::Bytes { chunk, read_at }) {
        Ok(()) => std::ops::ControlFlow::Continue(()),
        Err(TrySendError::Full(event)) => {
            // The actor is behind the child.
            crate::perf::PTY_READER_BLOCKED.incr();
            if tx.blocking_send(event).is_err() {
                return std::ops::ControlFlow::Break(());
            }
            std::ops::ControlFlow::Continue(())
        }
        Err(TrySendError::Closed(_)) => std::ops::ControlFlow::Break(()),
    }
}

/// Duplicate the master for the reader. `F_DUPFD_CLOEXEC` keeps the dup out of
/// pane children. `O_NONBLOCK` is a status flag on the shared open file
/// description, so [`super::park::set_nonblock`] applies to the writer too.
fn dup_master_reader(master: &dyn MasterPty) -> Result<std::fs::File, TerminalActorError> {
    use std::os::fd::{BorrowedFd, FromRawFd, OwnedFd};
    let raw = master
        .as_raw_fd()
        .ok_or_else(|| TerminalActorError::PtyIo("pty master has no file descriptor".to_owned()))?;
    // SAFETY: `raw` belongs to `master`, which the caller holds across this
    // dup. The borrow ends before `master` is used again.
    let borrowed = unsafe { BorrowedFd::borrow_raw(raw) };
    let fd = nix::fcntl::fcntl(borrowed, nix::fcntl::FcntlArg::F_DUPFD_CLOEXEC(0))
        .map_err(|err| TerminalActorError::PtyIo(err.to_string()))?;
    // SAFETY: `F_DUPFD_CLOEXEC` just created `fd` and this process owns it.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    Ok(std::fs::File::from(owned))
}

/// Shared tail of [`spawn_pty`] / [`adopt_pty`]: start the bridge threads and
/// assemble the [`PtyOwned`] bundle and channel endpoints.
fn start_pty_bridge(
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
) -> Result<SpawnedPty, TerminalActorError> {
    let reader = dup_master_reader(&*master)?;
    // One open file description backs the master, reader, and writer.
    // `O_NONBLOCK` is shared, so writes wait out `EAGAIN` instead of
    // treating it as fatal. Only the reader consumes output.
    super::park::set_nonblock(&reader).map_err(|err| TerminalActorError::PtyIo(err.to_string()))?;
    let writer = master
        .take_writer()
        .map_err(|e| TerminalActorError::PtyIo(e.to_string()))?;
    let master = Arc::new(Mutex::new(master));
    // The writer inspects termios before writes; `PtyOwned::master` keeps the
    // other clone for resize ioctls.
    let master_for_writer = Arc::clone(&master);

    let (pty_tx_to_actor, pty_rx_for_actor) = mpsc::channel::<PtyEvent>(PTY_CHANNEL_DEPTH);
    let (input_tx_to_writer, input_rx_for_writer) =
        mpsc::channel::<EncodedInputRequest>(super::PTY_WRITER_QUEUE);

    let reader = super::park::attach(reader, pty_tx_to_actor);
    let fast =
        super::fast_pty::PtyFastPath::new(input_tx_to_writer.clone(), master_for_writer, writer);
    let writer_fast = Arc::clone(&fast);
    let writer_thread = std::thread::Builder::new()
        .name("phux-pty-writer".to_owned())
        .stack_size(super::park::READER_STACK)
        .spawn(move || writer_fast.run_writer(input_rx_for_writer))
        .map_err(|e| TerminalActorError::PtyIo(e.to_string()))?;

    Ok((
        pty_rx_for_actor,
        input_tx_to_writer,
        PtyOwned {
            master,
            child,
            reader,
            writer_thread: Some(writer_thread),
            fast,
        },
    ))
}

#[cfg(test)]
mod exit_outcome_tests;

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod writer_tests;

#[cfg(test)]
mod tests;

/// Canonical-mode write guard (phux-mjmc): the pure predicate, then real-PTY
/// tests through the production writer thread.
#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod canonical_guard_tests;

#[cfg(test)]
mod drain_tests;
