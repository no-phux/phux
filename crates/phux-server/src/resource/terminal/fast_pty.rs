//! Inline PTY write for a keystroke the receive task already holds.
//!
//! The writer thread still owns every queued request. A legacy printable
//! key may write the master on the receive task when nothing is queued and
//! the writer is blocked in `recv`, then nonblocking-read the kernel echo
//! and hand it to the actor on the same channel the reader uses. The two
//! sides share one phase bit so a queued byte is never written second.
//!
//! A key that missed the inline path is counted in `lane_ahead` before it
//! is handed to the input lane. `pending` stays zero until that lane
//! encodes and enqueues, so without the count the next printable would
//! write ahead of Enter or Ctrl-U.

#![allow(
    clippy::redundant_pub_crate,
    reason = "pub(crate) module: unreachable_pub wants pub(crate) items, and redundant_pub_crate wants pub"
)]

use std::fs::File;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use bytes::Bytes;
use portable_pty::MasterPty;
use tokio::sync::mpsc;

use super::EncodedInputRequest;
use super::INPUT_CREDITS;
use super::park::note_read;
use super::spawn::{self, PtyEvent, WriterLoopControl, send_pty_chunk, service_write_request};

const IDLE: u8 = 0;
const BUSY: u8 = 1;

/// What [`PtyFastPath::try_direct`] did with the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectWrite {
    /// `write(2)` and `flush` succeeded, and the echo was stolen if it was
    /// already sitting in the master.
    Wrote,
    /// Refused or failed before a retry would help. The caller drops the key.
    Handled,
    /// The writer thread owns the next byte. The caller uses the lane.
    Busy,
}

struct WriterSlot {
    writer: Option<std::mem::ManuallyDrop<Box<dyn Write + Send>>>,
}

/// Held from the moment a key is queued to the input lane until the writer
/// queue has counted it (or the key is dropped). Dropping this is the
/// decrement.
pub(crate) struct LaneAhead {
    fast: Arc<PtyFastPath>,
}

impl LaneAhead {
    /// Count one in-flight lane key. The caller keeps the guard until
    /// [`PtyFastPath::enqueue`] has published `pending`, or until the key
    /// is abandoned.
    ///
    /// The increment takes the same lock as [`PtyFastPath::try_direct`]'s
    /// check, so a key cannot pass the check while this count is moving
    /// from zero to one.
    pub(crate) fn arm(fast: &Arc<PtyFastPath>) -> Self {
        let _admit = lock(&fast.admit);
        fast.lane_ahead.fetch_add(1, Ordering::AcqRel);
        Self {
            fast: Arc::clone(fast),
        }
    }
}

impl Drop for LaneAhead {
    fn drop(&mut self) {
        let mut current = self.fast.lane_ahead.load(Ordering::Acquire);
        loop {
            if current == 0 {
                debug_assert!(false, "lane_ahead underflow");
                return;
            }
            match self.fast.lane_ahead.compare_exchange_weak(
                current,
                current - 1,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }
}

impl std::fmt::Debug for LaneAhead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LaneAhead")
    }
}

/// Shared PTY writer gate for one pane.
pub(crate) struct PtyFastPath {
    phase: AtomicU8,
    pending: AtomicUsize,
    /// Keys accepted by the input lane and not yet counted in `pending`.
    lane_ahead: AtomicUsize,
    closed: AtomicBool,
    /// Admission lock, holding this path's sender into the writer queue.
    /// [`Self::close`] takes it so the writer thread sees its channel close
    /// once the actor drops its own sender.
    admit: Mutex<Option<mpsc::Sender<EncodedInputRequest>>>,
    cv: Condvar,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    io: Mutex<WriterSlot>,
    echo_read: Mutex<File>,
    pty_tx: mpsc::Sender<PtyEvent>,
}

impl PtyFastPath {
    pub(super) fn new(
        input_tx: mpsc::Sender<EncodedInputRequest>,
        master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
        echo_read: File,
        pty_tx: mpsc::Sender<PtyEvent>,
        writer: Box<dyn Write + Send>,
    ) -> Arc<Self> {
        Arc::new(Self {
            phase: AtomicU8::new(IDLE),
            pending: AtomicUsize::new(0),
            lane_ahead: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            admit: Mutex::new(Some(input_tx)),
            cv: Condvar::new(),
            master,
            io: Mutex::new(WriterSlot {
                writer: Some(std::mem::ManuallyDrop::new(writer)),
            }),
            echo_read: Mutex::new(echo_read),
            pty_tx,
        })
    }

    /// Queue `request` for the writer thread, counting it until that thread
    /// has claimed the phase bit. Direct writes refuse while the count is
    /// nonzero.
    pub(crate) fn enqueue(
        &self,
        mut request: EncodedInputRequest,
    ) -> Result<(), mpsc::error::TrySendError<EncodedInputRequest>> {
        let admit = lock(&self.admit);
        let Some(input_tx) = admit.as_ref() else {
            return Err(mpsc::error::TrySendError::Closed(request));
        };
        if request.credit.is_none() && input_tx.capacity() <= INPUT_CREDITS {
            crate::perf::INPUT_WRITER_FULL.incr();
            return Err(mpsc::error::TrySendError::Full(request));
        }
        // Drop the lane count only after `pending` is visible, and only
        // while this lock is held, so `try_direct` cannot see both at zero.
        let ahead = request.lane_ahead.take();
        self.pending.fetch_add(1, Ordering::AcqRel);
        request.counts_pending = true;
        request.writer_queued_at = Some(Instant::now());
        let sent = input_tx.try_send(request);
        if sent.is_err() {
            self.pending.fetch_sub(1, Ordering::Release);
        }
        drop(ahead);
        drop(admit);
        match sent {
            Ok(()) => Ok(()),
            Err(err) => {
                if matches!(&err, mpsc::error::TrySendError::Full(_)) {
                    crate::perf::INPUT_WRITER_FULL.incr();
                }
                Err(err)
            }
        }
    }

    /// Write `bytes` on this thread when the writer is idle in `recv`.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the admit lock covers both the idle checks and the phase claim"
    )]
    pub(crate) fn try_direct(&self, bytes: &[u8]) -> DirectWrite {
        if !phux_perf::echo_hop::fast_path_enabled() || self.closed.load(Ordering::Acquire) {
            return DirectWrite::Busy;
        }
        {
            let admit = lock(&self.admit);
            if admit.is_none()
                || self.pending.load(Ordering::Acquire) != 0
                || self.lane_ahead.load(Ordering::Acquire) != 0
                || self.phase.load(Ordering::Acquire) == BUSY
            {
                return DirectWrite::Busy;
            }
            if self
                .phase
                .compare_exchange(IDLE, BUSY, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return DirectWrite::Busy;
            }
        }
        let outcome = self.write_direct(bytes);
        if outcome == DirectWrite::Wrote {
            self.steal_echo();
        }
        self.release_busy();
        outcome
    }

    /// Writer thread: block on `rx`, and never write while a direct write
    /// holds the phase bit.
    pub(super) fn run_writer(self: &Arc<Self>, mut rx: mpsc::Receiver<EncodedInputRequest>) {
        crate::perf::promote_helper_thread("phux-pty-writer");
        loop {
            let Some(request) = rx.blocking_recv() else {
                self.finish_clean();
                return;
            };
            if !self.claim_busy() {
                self.finish_clean();
                return;
            }
            if request.counts_pending {
                self.pending.fetch_sub(1, Ordering::Release);
            }
            let control = self.service(request);
            self.release_busy();
            if control == WriterLoopControl::Stop {
                self.abandon_writer();
                return;
            }
        }
    }

    fn release_busy(&self) {
        let _admit = lock(&self.admit);
        self.phase.store(IDLE, Ordering::Release);
        self.cv.notify_all();
    }

    fn claim_busy(&self) -> bool {
        let mut admit = lock(&self.admit);
        loop {
            if self.closed.load(Ordering::Acquire) {
                return false;
            }
            if self
                .phase
                .compare_exchange(IDLE, BUSY, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
            admit = self.cv.wait(admit).unwrap_or_else(PoisonError::into_inner);
        }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the writer borrow lives inside the mutex guard; dropping the guard before service_write_request would free the writer"
    )]
    fn service(&self, request: EncodedInputRequest) -> WriterLoopControl {
        let mut io = lock(&self.io);
        let Some(writer) = io.writer.as_mut() else {
            drop(io);
            if let Some(completion) = request.completion {
                completion.complete(super::WriteCompletion::NotWritten);
            }
            return WriterLoopControl::Continue;
        };
        service_write_request(&mut **writer, &self.master, request)
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the writer borrow lives inside the mutex guard for the whole write and flush"
    )]
    fn write_direct(&self, bytes: &[u8]) -> DirectWrite {
        let mut io = lock(&self.io);
        let Some(writer) = io.writer.as_mut() else {
            return DirectWrite::Busy;
        };
        if let Some(overflow) = spawn::canonical_refusal(&self.master, bytes) {
            crate::perf::INPUT_CANONICAL_REFUSED.incr();
            tracing::error!(
                len = bytes.len(),
                limit = overflow.limit,
                "pty fast path: refusing write; pane is in canonical mode and this \
                 payload has no line terminator within its canonical-line limit"
            );
            return DirectWrite::Handled;
        }
        let started = Instant::now();
        let outcome = spawn::write_all_pty(&mut **writer, &self.master, bytes)
            .and_then(|()| spawn::flush_pty(&mut **writer, &self.master));
        crate::perf::INPUT_PTY_WRITE.record_elapsed(started);
        match outcome {
            Ok(()) => {
                phux_perf::echo_hop::record("pty_write");
                crate::perf::INPUT_EVENTS.incr();
                DirectWrite::Wrote
            }
            Err(err) => {
                tracing::debug!(?err.failure, "pty fast path write failed");
                DirectWrite::Handled
            }
        }
    }

    /// Nonblocking read of the echo just written. The hot reader uses the
    /// other dup; exactly one `read(2)` consumes the bytes.
    fn steal_echo(&self) {
        let mut file = lock(&self.echo_read);
        let mut buf = [0_u8; 1024];
        match file.read(&mut buf) {
            Ok(n) if n > 0 => {
                note_read(n);
                let chunk = Bytes::copy_from_slice(&buf[..n]);
                let _ = send_pty_chunk(&self.pty_tx, chunk, Instant::now());
            }
            _ => {}
        }
    }

    /// Refuse further direct writes and enqueues, and give up this path's
    /// sender so the writer thread drains what is queued and ends once the
    /// actor drops its own. Without this the writer would wait on a channel
    /// this struct keeps open, and keep the master and the echo descriptor
    /// alive with it.
    pub(super) fn close(&self) {
        let sender = lock(&self.admit).take();
        drop(sender);
    }

    fn finish_clean(&self) {
        self.closed.store(true, Ordering::Release);
        let mut io = lock(&self.io);
        if let Some(writer) = io.writer.take() {
            drop(std::mem::ManuallyDrop::into_inner(writer));
        }
    }

    fn abandon_writer(&self) {
        self.closed.store(true, Ordering::Release);
        let mut io = lock(&self.io);
        if let Some(writer) = io.writer.take() {
            std::mem::forget(std::mem::ManuallyDrop::into_inner(writer));
        }
    }
}

impl std::fmt::Debug for PtyFastPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyFastPath")
            .field("pending", &self.pending.load(Ordering::Relaxed))
            .field("lane_ahead", &self.lane_ahead.load(Ordering::Relaxed))
            .field("phase", &self.phase.load(Ordering::Relaxed))
            .field("closed", &self.closed.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
