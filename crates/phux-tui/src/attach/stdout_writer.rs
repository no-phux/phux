//! Off-loop stdout writer.
//!
//! A blocking flush to a slow tty inside the `select!` loop would starve the
//! stdin/signal arms (Ctrl-C and detach stop working). [`StdoutSink`] is the
//! driver's `out`: writes accumulate in memory, and `flush()` hands the frame
//! to a dedicated thread that owns the real stdout.
//!
//! Backpressure is bounded and lossless at the frame level: once the queued
//! backlog exceeds [`CAP_BYTES`] the sink drops it and sets `needs_resync`,
//! and the driver repaints a self-contained full frame (`ED2` + redraw) that
//! supersedes every dropped diff.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

/// Backlog cap: how much ALREADY-QUEUED work may pile up before the sink
/// drops it and forces a resync. It never governs the frame in hand (see
/// [`StdoutSink::flush`]).
pub(super) const CAP_BYTES: usize = 256 * 1024;

/// Shared producer/consumer state behind the lock.
struct QueueState {
    /// Complete-`flush()` byte buffers, written to the tty in order.
    chunks: VecDeque<Vec<u8>>,
    /// Buffers the writer finished with, returned for the sink to refill so
    /// the steady state neither allocates nor frees.
    spare: Vec<Vec<u8>>,
    /// Running total of `chunks` byte lengths (cheap cap check).
    bytes: usize,
    /// Largest recycled buffer either side may pool, published by the sink.
    spare_limit: usize,
    /// Set by [`WriterHandle::shutdown_and_join`]; tells the writer to drain
    /// and exit.
    shutdown: bool,
}

/// Upper bound on recycled buffers held between the two sides (one in
/// flight, one being filled).
const SPARE_POOL: usize = 2;

/// Floor for the recycled-buffer size limit.
const SPARE_MIN_BYTES: usize = 64 * 1024;

/// Ceiling on a recycled buffer, so a one-off giant frame cannot pin memory.
/// The applied limit is the largest chunk shipped, clamped to
/// `[SPARE_MIN_BYTES, SPARE_MAX_BYTES]`.
const SPARE_MAX_BYTES: usize = 1024 * 1024;

struct Shared {
    queue: Mutex<QueueState>,
    cv: Condvar,
}

/// The `Write` the driver threads through `main_loop` as `out`. `write*`
/// only appends; `flush` ships the frame and wakes the writer.
pub(super) struct StdoutSink {
    shared: Arc<Shared>,
    /// Set when the backlog overflowed and stale frames were dropped; the
    /// driver polls it and repaints.
    pub(super) needs_resync: Arc<AtomicBool>,
    pending: Vec<u8>,
    /// Buffers reclaimed from the writer thread.
    recycled: Vec<Vec<u8>>,
    /// Largest chunk capacity shipped (monotone), which sizes the recycling
    /// limit.
    high_water: usize,
}

impl StdoutSink {
    /// Take back buffers the writer finished with (queue lock held).
    fn reclaim(recycled: &mut Vec<Vec<u8>>, q: &mut QueueState) {
        let limit = q.spare_limit;
        while recycled.len() < SPARE_POOL {
            let Some(mut buf) = q.spare.pop() else { break };
            if buf.capacity() > limit {
                continue;
            }
            buf.clear();
            recycled.push(buf);
        }
        q.spare.clear();
    }

    /// Return `buf` to `pool` if there is room and its CAPACITY is within
    /// `limit`. Both sides pool through this one rule.
    fn pool(pool: &mut Vec<Vec<u8>>, mut buf: Vec<u8>, limit: usize) {
        if pool.len() >= SPARE_POOL || buf.capacity() > limit {
            return;
        }
        buf.clear();
        pool.push(buf);
    }
}

impl Write for StdoutSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.pending.extend_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        phux_client::perf::FLUSHES.add(1);
        phux_client::perf::BYTES_OUT.add(u64::try_from(self.pending.len()).unwrap_or(u64::MAX));
        {
            let mut q = self
                .shared
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Self::reclaim(&mut self.recycled, &mut q);
            let recycled = self.recycled.pop().unwrap_or_default();
            let chunk = std::mem::replace(&mut self.pending, recycled);
            // Sized from CAPACITY, which is what the pooling rule tests: a
            // doubled-growth 400 KB frame sits in a 512 KiB allocation, and a
            // limit from `len` rejected every such buffer.
            self.high_water = self.high_water.max(chunk.capacity());
            q.spare_limit = self.high_water.clamp(SPARE_MIN_BYTES, SPARE_MAX_BYTES);
            // The cap governs the EXISTING BACKLOG, never the frame in hand: a
            // single frame over the cap (a big truecolor repaint) must still
            // land, or the resync repaint that answers it is refused forever.
            if q.bytes > CAP_BYTES {
                // A real backlog: drop it, and the frame in hand too (its diff
                // assumes the dropped bytes landed). The self-contained resync
                // repaint then lands on an empty queue.
                q.chunks.clear();
                q.bytes = 0;
                self.needs_resync.store(true, Ordering::Release);
                phux_client::perf::STDOUT_DROPS.incr();
                if let Some(suppressed) = phux_client::perf::STDOUT_DROP_WARN.admit() {
                    tracing::warn!(
                        dropped_bytes = chunk.len(),
                        suppressed,
                        "stdout backlog over cap; dropping queued diffs and resyncing (the outer terminal is not keeping up)",
                    );
                }
                Self::pool(&mut self.recycled, chunk, q.spare_limit);
            } else {
                q.bytes += chunk.len();
                q.chunks.push_back(chunk);
            }
        }
        self.shared.cv.notify_one();
        Ok(())
    }
}

/// Owns the writer thread; used to drain + stop it cleanly on attach exit.
pub(super) struct WriterHandle {
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
}

impl WriterHandle {
    /// Stop the writer and join it, DROPPING any queued backlog: every exit
    /// path leaves the alt screen, so draining to a slow terminal would only
    /// make detach hang. Call before the reset writes on every exit path.
    pub(super) fn shutdown_and_join(mut self) {
        {
            let mut q = self
                .shared
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            q.shutdown = true;
            q.chunks.clear();
            q.bytes = 0;
        }
        self.shared.cv.notify_one();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Spawn the stdout writer thread and return the sink + its handle.
pub(super) fn spawn_stdout_writer() -> (StdoutSink, WriterHandle) {
    spawn_writer_into(io::stdout())
}

/// As [`spawn_stdout_writer`] but writing to an arbitrary sink (tests).
#[allow(
    clippy::expect_used,
    reason = "thread spawn failure at attach start is fatal and unrecoverable"
)]
fn spawn_writer_into<W: Write + Send + 'static>(inner: W) -> (StdoutSink, WriterHandle) {
    let shared = Arc::new(Shared {
        queue: Mutex::new(QueueState {
            chunks: VecDeque::new(),
            spare: Vec::new(),
            bytes: 0,
            spare_limit: SPARE_MIN_BYTES,
            shutdown: false,
        }),
        cv: Condvar::new(),
    });
    let writer_shared = Arc::clone(&shared);
    let join = std::thread::Builder::new()
        .name("phux-stdout".to_owned())
        .spawn(move || {
            let _ = phux_perf::promote_current_thread();
            writer_loop(&writer_shared, inner);
        })
        .expect("spawn phux-stdout writer thread");
    let sink = StdoutSink {
        shared: Arc::clone(&shared),
        needs_resync: Arc::new(AtomicBool::new(false)),
        pending: Vec::with_capacity(8192),
        recycled: Vec::with_capacity(SPARE_POOL),
        high_water: 0,
    };
    (
        sink,
        WriterHandle {
            shared,
            join: Some(join),
        },
    )
}

/// Drain the queue to `out` off the runtime thread; exits once `shutdown` is
/// set and the queue is empty.
fn writer_loop<W: Write>(shared: &Shared, mut out: W) {
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    loop {
        {
            let mut q = shared
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while q.chunks.is_empty() && !q.shutdown {
                q = shared
                    .cv
                    .wait(q)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if q.chunks.is_empty() && q.shutdown {
                break;
            }
            q.bytes = 0;
            chunks.extend(q.chunks.drain(..));
        }
        for chunk in &chunks {
            if out.write_all(chunk).is_err() {
                return;
            }
        }
        let _ = out.flush();
        {
            let mut q = shared
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let limit = q.spare_limit;
            #[allow(
                clippy::iter_with_drain,
                reason = "`chunks` is reused across loop iterations; `into_iter` would consume the allocation this loop exists to keep"
            )]
            for buf in chunks.drain(..) {
                StdoutSink::pool(&mut q.spare, buf, limit);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// A sink with no writer thread, so the queue only grows (a stuck
    /// terminal), starting from `spare`.
    fn detached_sink(spare: Vec<Vec<u8>>) -> StdoutSink {
        StdoutSink {
            shared: Arc::new(Shared {
                queue: Mutex::new(QueueState {
                    chunks: VecDeque::new(),
                    spare,
                    bytes: 0,
                    spare_limit: SPARE_MIN_BYTES,
                    shutdown: false,
                }),
                cv: Condvar::new(),
            }),
            needs_resync: Arc::new(AtomicBool::new(false)),
            pending: Vec::new(),
            recycled: Vec::new(),
            high_water: 0,
        }
    }

    fn ship(sink: &mut StdoutSink, len: usize) -> (usize, usize, bool) {
        sink.write_all(&vec![b'#'; len]).expect("write");
        sink.flush().expect("flush");
        let q = sink.shared.queue.lock().expect("lock");
        (
            q.chunks.len(),
            q.bytes,
            sink.needs_resync.load(Ordering::Acquire),
        )
    }

    /// A writer that discards, so buffers come straight back.
    struct Discard;
    impl Write for Discard {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct BlockingSink {
        control: Arc<(Mutex<(bool, bool)>, Condvar)>,
    }

    impl Write for BlockingSink {
        /// Marks `(in_write, _)` and blocks until `(_, release)`.
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let (state, changed) = &*self.control;
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.0 = true;
            changed.notify_one();
            while !state.1 {
                state = changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            state.0 = false;
            drop(state);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The point of the thread: `flush` returns while the writer is blocked
    /// inside the terminal write.
    #[test]
    fn flush_does_not_block_on_a_slow_sink() {
        let control = Arc::new((Mutex::new((false, false)), Condvar::new()));
        let (mut sink, handle) = spawn_writer_into(BlockingSink {
            control: Arc::clone(&control),
        });
        sink.write_all(b"first frame").expect("write");
        sink.flush().expect("flush");
        let (state, changed) = &*control;
        let (guard, timeout) = changed
            .wait_timeout_while(
                state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                Duration::from_secs(2),
                |state| !state.0,
            )
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            !timeout.timed_out(),
            "writer never entered the blocked sink"
        );
        drop(guard);

        // Flush on a helper thread so a regression cannot wedge the test.
        let (flushed_tx, flushed_rx) = mpsc::channel();
        let flush_task = std::thread::spawn(move || {
            sink.write_all(b"second frame").expect("write");
            sink.flush().expect("flush while writer is blocked");
            flushed_tx.send(()).expect("report");
            sink
        });
        let flush_returned = flushed_rx.recv_timeout(Duration::from_secs(2)).is_ok();
        let mut guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let writer_stayed_blocked = guard.0 && !guard.1;
        guard.1 = true;
        changed.notify_one();
        drop(guard);
        drop(flush_task.join().expect("flush helper"));
        assert!(flush_returned && writer_stayed_blocked);
        handle.shutdown_and_join();
    }

    /// The cap governs the backlog, not the frame in hand: the chunk that
    /// crosses the cap still lands, and only the NEXT flush drops the backlog
    /// and asks for a resync.
    #[test]
    fn the_cap_drops_an_over_cap_backlog_on_the_next_flush() {
        let mut sink = detached_sink(Vec::new());
        assert_eq!(ship(&mut sink, CAP_BYTES - 1), (1, CAP_BYTES - 1, false));
        assert_eq!(ship(&mut sink, 3), (2, CAP_BYTES + 2, false));
        assert_eq!(ship(&mut sink, 1), (0, 0, true));

        // A backlog of small frames trips it too, and stays bounded.
        let mut sink = detached_sink(Vec::new());
        let mut bytes = 0;
        for _ in 0..40 {
            bytes = ship(&mut sink, 8 * 1024).1;
            assert!(bytes <= CAP_BYTES + 8 * 1024);
        }
        assert!(sink.needs_resync.load(Ordering::Acquire), "{bytes}");
    }

    /// Regression: a single frame over the cap (a 250x70 truecolor repaint)
    /// on an empty queue is written, and repeated ones keep reaching the
    /// queue; refusing them froze the screen after any full repaint.
    #[test]
    fn oversized_frames_are_written_not_dropped() {
        const BIG: usize = 300 * 1024;
        let mut sink = detached_sink(Vec::new());
        assert_eq!(ship(&mut sink, BIG), (1, BIG, false));
        let mut queued = 1;
        for _ in 0..5 {
            let (chunks, bytes, _) = ship(&mut sink, BIG);
            queued += chunks;
            assert!(
                bytes <= CAP_BYTES + BIG,
                "queue grew past its bound: {bytes}"
            );
        }
        assert!(queued >= 3, "only {queued} of 6 frames were queued");
    }

    /// A one-off giant buffer is dropped, not pooled.
    #[test]
    fn oversized_buffers_are_not_recycled() {
        let mut sink = detached_sink(vec![Vec::with_capacity(SPARE_MAX_BYTES + 1)]);
        let mut q = sink.shared.queue.lock().expect("lock");
        StdoutSink::reclaim(&mut sink.recycled, &mut q);
        drop(q);
        assert!(sink.recycled.is_empty());
    }

    /// Recycling engages for a large frame accumulated by many small writes
    /// (doubling growth): the limit is sized from capacity, so a frame-sized
    /// allocation survives in the pool instead of one being minted per frame.
    #[test]
    fn a_large_frames_buffer_is_reused_rather_than_reallocated() {
        const FRAME: usize = 300 * 1024;
        const PIECE: usize = 4 * 1024;
        let (mut sink, handle) = spawn_writer_into(Discard);
        let piece = vec![b'#'; PIECE];
        let mut pooled = false;
        for _ in 0..6 {
            for _ in 0..FRAME / PIECE {
                sink.write_all(&piece).expect("write");
            }
            sink.flush().expect("flush");
            std::thread::sleep(Duration::from_millis(20));
            let in_queue = sink
                .shared
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .spare
                .iter()
                .any(|b| b.capacity() >= FRAME);
            pooled |= in_queue
                || sink.recycled.iter().any(|b| b.capacity() >= FRAME)
                || sink.pending.capacity() >= FRAME;
        }
        let limit = sink.shared.queue.lock().expect("lock").spare_limit;
        assert!(pooled, "no frame-sized buffer survived; limit={limit}");
        assert!((FRAME..=SPARE_MAX_BYTES).contains(&limit), "limit={limit}");
        handle.shutdown_and_join();
    }
}
