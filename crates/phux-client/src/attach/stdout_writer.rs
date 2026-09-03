//! Off-loop stdout writer (phux-fysb).
//!
//! The attach `tokio::select!` loop renders synchronously: every
//! `paint_full_frame`/`render_at` ends in `out.flush()`. When `out` is the
//! real tty that flush BLOCKS until the terminal drains, and because the
//! paint happens *inside* the `biased` `conn.recv()` select arm, a slow
//! terminal starves the stdin/signal arms — the client wedges (Ctrl-C and
//! detach stop working). Multi-pane re-attach makes it worse: the
//! `paint_full_frame` burst is ~N× a single pane's bytes, so it crosses the
//! wedge threshold a single pane never reaches.
//!
//! [`StdoutSink`] breaks that coupling. It is a `Write` that the driver uses
//! as `out`: writes accumulate in an in-memory buffer, and `flush()` ships
//! the accumulated bytes to a dedicated OS thread that owns the real stdout
//! and does the blocking write off the runtime thread. The select loop never
//! blocks on the terminal, so input/signals are always serviced.
//!
//! Backpressure is bounded and lossless-at-the-frame-level: if the writer
//! falls far enough behind that the queued backlog exceeds [`CAP_BYTES`], the
//! sink DROPS the stale backlog and sets a `needs_resync` flag. The driver
//! polls that flag and repaints the latest state from scratch
//! (`paint_full_frame` is self-contained — an `ED2` clear + full redraw — so
//! it supersedes every dropped diff). The result under a sustained-slow sink:
//! the user sees the newest full frame as fast as the terminal can absorb it,
//! intermediate diffs are dropped, memory stays bounded, and the loop never
//! blocks.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

/// Backlog cap in bytes: how much ALREADY-QUEUED work may pile up before the
/// sink drops it and forces a resync.
///
/// This governs the backlog and never the frame in hand — see [`StdoutSink::flush`]
/// for why that distinction is load-bearing. It bounds memory under a stuck or
/// slow terminal; the queue's true ceiling is this plus the one frame being
/// enqueued when the cap trips.
pub(super) const CAP_BYTES: usize = 256 * 1024;

/// Shared producer/consumer state behind the lock.
struct QueueState {
    /// Complete-`flush()` byte buffers, written to the tty in order.
    chunks: VecDeque<Vec<u8>>,
    /// Buffers the writer thread has finished with, returned here for the
    /// sink to refill.
    ///
    /// Without this, every `flush()` handed its `Vec` to the writer and
    /// started a fresh allocation for the next frame — one malloc plus one
    /// free per frame forever, and the fresh buffer had to grow back to
    /// frame size from zero. Recycling makes the steady state allocation-free
    /// (both sides converge on buffers already large enough) while keeping
    /// the queue's ownership story unchanged: a buffer is either in `chunks`
    /// (owed to the terminal), in `spare` (owned by nobody), or in the sink's
    /// `pending`.
    spare: Vec<Vec<u8>>,
    /// Running total of `chunks` byte lengths (cheap cap check).
    bytes: usize,
    /// Largest recycled buffer the writer thread may return, published by the
    /// sink from the frame sizes it actually ships. Both sides apply the same
    /// limit, so a buffer is never pooled by one and rejected by the other.
    spare_limit: usize,
    /// Set by [`WriterHandle::shutdown_and_join`]; tells the writer to drain
    /// and exit.
    shutdown: bool,
}

/// Upper bound on recycled buffers held between the two sides.
///
/// One in flight and one being filled is the steady state, which is what this
/// is sized to now that a spare may be frame-sized rather than capped at a
/// flat 64 KiB: two big-viewport buffers is a bounded amount of memory, four
/// was not.
const SPARE_POOL: usize = 2;

/// Floor for the recycled-buffer size limit — always worth pooling this much,
/// even before a large frame has been seen.
const SPARE_MIN_BYTES: usize = 64 * 1024;

/// Absolute ceiling on a recycled buffer, so a pathological one-off frame
/// cannot pin memory forever.
///
/// The limit actually applied is the largest chunk this sink has shipped,
/// clamped between [`SPARE_MIN_BYTES`] and this. A flat 64 KiB ceiling meant
/// recycling disengaged exactly where an allocation per frame costs most: a
/// 250x70 truecolor repaint is ~400 KB, so every such frame allocated a fresh
/// buffer and freed the old one while the pool sat unused.
const SPARE_MAX_BYTES: usize = 1024 * 1024;

struct Shared {
    queue: Mutex<QueueState>,
    cv: Condvar,
}

/// The `Write` the driver threads through `main_loop` as `out`.
///
/// `write*` only appends to `pending` (never blocks, never locks). `flush`
/// is the ship point: it moves `pending` into the shared queue and wakes the
/// writer thread.
pub(super) struct StdoutSink {
    shared: Arc<Shared>,
    /// Driver-polled: set when the backlog overflowed and stale frames were
    /// dropped, so the driver repaints the latest state. Cloned so the driver
    /// can hold a reader independent of the `&mut StdoutSink` borrow.
    pub(super) needs_resync: Arc<AtomicBool>,
    pending: Vec<u8>,
    /// Buffers reclaimed from the writer thread, ready to be refilled.
    recycled: Vec<Vec<u8>>,
    /// Largest chunk this sink has shipped, which is what the recycling limit
    /// is sized from. Monotone: a viewport that shrinks keeps the larger
    /// limit, which costs at most [`SPARE_POOL`] buffers of memory and avoids
    /// thrashing the pool across a resize.
    high_water: usize,
}

impl StdoutSink {
    /// Take back buffers the writer thread has finished with.
    ///
    /// Called with the queue lock already held (the flush path takes it
    /// anyway), so recycling costs no extra synchronization. An associated
    /// function rather than a method so the caller can hold the lock guard —
    /// which borrows `self.shared` — while handing over `self.recycled`.
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
        // Anything left in `spare` beyond what we want is dropped here rather
        // than accumulating.
        q.spare.clear();
    }

    /// Return `buf` to `pool` if there is room for it and it is not larger
    /// than `limit`. Both sides of the queue pool through the same rule.
    /// `limit` is compared against CAPACITY — the memory the buffer actually
    /// pins — and is itself derived from capacity in [`StdoutSink::flush`], so
    /// the two cannot disagree about what a frame-sized buffer measures.
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
        crate::attach::render_prof::note_flushes(1);
        crate::attach::render_prof::note_bytes(
            u64::try_from(self.pending.len()).unwrap_or(u64::MAX),
        );
        {
            let mut q = self
                .shared
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Reclaim first, under the lock we had to take anyway, so THIS
            // flush can refill a returned buffer rather than waiting a frame.
            Self::reclaim(&mut self.recycled, &mut q);
            // Swap the filled buffer out for a recycled one, so the steady
            // state neither allocates nor frees. `mem::replace` keeps
            // `pending` a valid empty buffer at all times.
            let recycled = self.recycled.pop().unwrap_or_default();
            let chunk = std::mem::replace(&mut self.pending, recycled);
            // Grow the recycling limit to the frames this terminal actually
            // produces, bounded, and publish it for the writer side.
            //
            // Sized from CAPACITY, not length, because capacity is what the
            // pooling rule tests. `Vec` grows by doubling, so a 400 KB frame
            // ends up in a 512 KiB allocation: a limit taken from `len` sat at
            // 409600 while every buffer to be pooled measured 524288, so the
            // rule rejected every one of them and the sink reallocated the
            // frame buffer on every single frame — recycling that never
            // engaged for exactly the frames it was widened to catch.
            self.high_water = self.high_water.max(chunk.capacity());
            q.spare_limit = self.high_water.clamp(SPARE_MIN_BYTES, SPARE_MAX_BYTES);
            // The cap governs the EXISTING BACKLOG, never the frame in hand.
            //
            // It used to read `q.bytes + chunk.len() > CAP_BYTES`, which made
            // a single frame larger than the cap undroppable-into: with an
            // empty queue the sum is still over, so the frame was discarded
            // and `needs_resync` set; the driver answered with
            // `paint_full_frame`, which produces the SAME oversized chunk,
            // which was discarded again. A 250x70 truecolor repaint is around
            // 400 KB — one `chafa` render, or `btop`'s gradients — so the
            // screen simply stopped updating after any full repaint (resize,
            // split, overlay dismiss) while every wake-up burned a full
            // render. Making the frame in hand unconditional at an
            // under-cap queue is what breaks that loop: a fresh frame is
            // never the stale diff the cap exists to drop.
            if q.bytes > CAP_BYTES {
                // A real backlog: the writer is behind and these queued diffs
                // will never reach the glass. Drop them and ask the driver for
                // a self-contained repaint.
                //
                // The frame in hand goes too, and only here: dropping the
                // backlog is what CREATES the gap, and this chunk's diff was
                // computed against the state those dropped bytes would have
                // produced. Applying it over the gap would paint garbage. The
                // resync repaint that follows is self-contained (`ED2` plus a
                // full redraw) and lands on an empty queue, so it is enqueued
                // by the branch below and the screen converges.
                q.chunks.clear();
                q.bytes = 0;
                self.needs_resync.store(true, Ordering::Release);
                crate::perf::STDOUT_DROPS.incr();
                if let Some(suppressed) = crate::perf::STDOUT_DROP_WARN.admit() {
                    tracing::warn!(
                        dropped_bytes = chunk.len(),
                        suppressed,
                        "stdout backlog over cap; dropping queued diffs and resyncing (the outer terminal is not keeping up)",
                    );
                }
                // The dropped chunk's allocation is still useful; keep it
                // rather than freeing it on the very path where the sink is
                // under the most pressure.
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
    /// Stop the writer and join it. DROPS any queued backlog rather than
    /// draining it: every attach-exit path leaves the alt screen (the reset in
    /// `exit_after_detach` / `RawModeGuard::Drop`), which discards the
    /// alt-screen content the backlog was painting — so draining it to a slow
    /// terminal would just make detach hang for no visible benefit. The writer
    /// finishes at most the one chunk it is mid-write on, then exits; the
    /// direct reset write that follows is therefore not garbled by a queued
    /// frame. Call this BEFORE the reset writes on every exit path.
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

/// As [`spawn_stdout_writer`] but writes to an arbitrary sink — the seam tests
/// use to drive a deliberately-slow inner writer and prove `flush()` stays
/// non-blocking regardless of how slow the terminal is.
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
        .spawn(move || writer_loop(&writer_shared, inner))
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

/// Drain the queue to `out`, blocking on the sink off the runtime thread.
/// Exits once `shutdown` is set AND the queue is empty (so a clean shutdown
/// flushes every queued chunk first; `shutdown_and_join` clears the backlog so
/// this exits promptly).
fn writer_loop<W: Write>(shared: &Shared, mut out: W) {
    // Reused across iterations so the drain itself stops allocating a fresh
    // `Vec<Vec<u8>>` per wake-up.
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
        // Hand the emptied buffers back for the sink to refill.
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
    use std::time::{Duration, Instant};

    /// A sink that sleeps on every write — stands in for a terminal so slow it
    /// would wedge the select loop if `flush()` blocked on it.
    struct SlowSink {
        per_write: Duration,
    }
    impl Write for SlowSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            std::thread::sleep(self.per_write);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn flush_does_not_block_on_a_slow_sink() {
        // The writer thread sleeps 50ms per chunk; the select-loop-side
        // `flush()` must still return ~instantly. This is the core of the
        // phux-fysb fix: render never blocks on the terminal.
        let (mut sink, handle) = spawn_writer_into(SlowSink {
            per_write: Duration::from_millis(50),
        });
        let mut worst = Duration::ZERO;
        for i in 0..20u32 {
            sink.write_all(format!("frame-{i}\n").as_bytes())
                .expect("write");
            let t0 = Instant::now();
            sink.flush().expect("flush");
            worst = worst.max(t0.elapsed());
        }
        // 20 frames behind a 50ms/chunk sink is ~1s of writer work, but each
        // flush returned in well under one chunk-time. Generous bound to stay
        // robust on a loaded CI box; the unfixed (direct-stdout) path would
        // see flushes of ~50ms+ each.
        assert!(
            worst < Duration::from_millis(25),
            "flush blocked on the slow sink: worst={worst:?}"
        );
        handle.shutdown_and_join();
    }

    #[test]
    fn flush_never_blocks_and_ships_in_order() {
        let (mut sink, handle) = spawn_stdout_writer();
        // write+flush a few frames; flush must return immediately.
        for i in 0..5u8 {
            sink.write_all(&[i]).expect("write");
            sink.flush().expect("flush");
        }
        // Nothing dropped (well under the cap), resync not set.
        assert!(!sink.needs_resync.load(Ordering::Acquire));
        handle.shutdown_and_join();
    }

    #[test]
    fn overflow_drops_backlog_and_sets_resync() {
        // Drive the queue past CAP_BYTES WITHOUT a draining writer by building
        // the shared state directly (no thread), exercising the sink's flush
        // backpressure branch deterministically.
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
        let mut sink = StdoutSink {
            shared: Arc::clone(&shared),
            needs_resync: Arc::new(AtomicBool::new(false)),
            pending: Vec::new(),
            recycled: Vec::new(),
            high_water: 0,
        };
        // Queue just under the cap.
        sink.write_all(&vec![0u8; CAP_BYTES - 1]).expect("write");
        sink.flush().expect("flush");
        assert!(!sink.needs_resync.load(Ordering::Acquire));
        assert_eq!(shared.queue.lock().expect("lock").chunks.len(), 1);
        // The next chunk is ACCEPTED even though it carries the queue over
        // the cap. The boundary moved by exactly one flush when the cap was
        // narrowed to the backlog alone, and that is the whole point: the
        // frame in hand is never the stale diff the cap exists to drop, so it
        // is enqueued and the queue is left over-cap for the next flush to
        // notice.
        sink.write_all(&[1u8, 2, 3]).expect("write");
        sink.flush().expect("flush");
        assert!(
            !sink.needs_resync.load(Ordering::Acquire),
            "the chunk that crosses the cap still lands"
        );
        assert_eq!(shared.queue.lock().expect("lock").chunks.len(), 2);
        // NOW the backlog is over the cap, so the following flush drops it.
        sink.write_all(&[4u8]).expect("write");
        sink.flush().expect("flush");
        assert!(sink.needs_resync.load(Ordering::Acquire));
        let (chunks_empty, bytes) = {
            let q = shared.queue.lock().expect("lock");
            (q.chunks.is_empty(), q.bytes)
        };
        assert!(chunks_empty, "stale backlog dropped on overflow");
        assert_eq!(bytes, 0);
    }

    /// The steady state must stop allocating: once the writer has returned a
    /// buffer, the next `flush()` refills that same allocation instead of
    /// handing the writer a fresh `Vec` and freeing the old one.
    #[test]
    fn flush_reuses_the_writers_returned_buffers() {
        let (mut sink, handle) = spawn_stdout_writer();
        // Prime the pool: one frame out, drained and returned by the writer.
        sink.write_all(&vec![b'x'; 4096]).expect("write");
        sink.flush().expect("flush");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let returned = {
                let q = sink
                    .shared
                    .queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                !q.spare.is_empty()
            };
            if returned {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // The next flush reclaims it: the sink ends up holding a warm buffer
        // rather than a freshly-allocated empty one.
        sink.write_all(b"second frame").expect("write");
        sink.flush().expect("flush");
        assert!(
            sink.pending.capacity() >= 4096,
            "flush must refill a recycled buffer, not allocate a new one \
             (capacity {})",
            sink.pending.capacity()
        );
        handle.shutdown_and_join();
    }

    /// A one-off giant frame must not pin its capacity in the pool forever.
    #[test]
    fn oversized_buffers_are_not_recycled() {
        let shared = Arc::new(Shared {
            queue: Mutex::new(QueueState {
                chunks: VecDeque::new(),
                spare: vec![Vec::with_capacity(SPARE_MAX_BYTES + 1)],
                bytes: 0,
                spare_limit: SPARE_MIN_BYTES,
                shutdown: false,
            }),
            cv: Condvar::new(),
        });
        let mut sink = StdoutSink {
            shared: Arc::clone(&shared),
            needs_resync: Arc::new(AtomicBool::new(false)),
            pending: Vec::new(),
            recycled: Vec::new(),
            high_water: 0,
        };
        {
            let mut q = shared.queue.lock().expect("lock");
            StdoutSink::reclaim(&mut sink.recycled, &mut q);
        }
        assert!(
            sink.recycled.is_empty(),
            "an oversized buffer must be dropped, not pooled"
        );
    }

    /// The regression this file exists to prevent shipping twice.
    ///
    /// Once the renderer stopped flushing per pane, `paint_full_frame`
    /// accumulated the whole viewport into ONE chunk. A 250x70 truecolor
    /// repaint is around 400 KB, which is over `CAP_BYTES` on its own — and
    /// the old check summed the queue with the incoming chunk, so an EMPTY
    /// queue still refused it. The driver answered `needs_resync` with
    /// `paint_full_frame`, which produced the same oversized chunk, which was
    /// refused again: the screen stopped updating after any full repaint
    /// while every wake-up burned a full render.
    #[test]
    fn an_oversized_frame_on_an_empty_queue_is_written_not_dropped() {
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
        let mut sink = StdoutSink {
            shared: Arc::clone(&shared),
            needs_resync: Arc::new(AtomicBool::new(false)),
            pending: Vec::new(),
            recycled: Vec::new(),
            high_water: 0,
        };
        // Larger than CAP_BYTES: one big-viewport truecolor frame.
        let frame = vec![b'#'; 300 * 1024];
        sink.write_all(&frame).expect("write");
        sink.flush().expect("flush");

        let q = shared.queue.lock().expect("lock");
        assert_eq!(q.chunks.len(), 1, "the frame must be queued, not dropped");
        assert_eq!(q.bytes, frame.len());
        drop(q);
        assert!(
            !sink.needs_resync.load(Ordering::Acquire),
            "a frame the terminal has not fallen behind on is not a resync"
        );
    }

    /// Repeated oversized frames still make progress: each lands on a queue
    /// the previous one left over-cap, so the sink alternates between
    /// enqueuing and asking for a resync — but it never refuses two in a row,
    /// which is what the freeze was.
    #[test]
    fn repeated_oversized_frames_keep_reaching_the_queue() {
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
        let mut sink = StdoutSink {
            shared: Arc::clone(&shared),
            needs_resync: Arc::new(AtomicBool::new(false)),
            pending: Vec::new(),
            recycled: Vec::new(),
            high_water: 0,
        };
        let mut queued = 0usize;
        for _ in 0..6 {
            sink.write_all(&vec![b'#'; 300 * 1024]).expect("write");
            sink.flush().expect("flush");
            let q = shared.queue.lock().expect("lock");
            queued += q.chunks.len();
            drop(q);
        }
        assert!(
            queued >= 3,
            "with a stuck writer at least every other frame must still be \
             queued; only {queued} of 6 were"
        );
        // Memory stayed bounded: the queue never holds more than the cap plus
        // the one frame that tripped it.
        let bytes = {
            let q = shared.queue.lock().expect("lock");
            q.bytes
        };
        assert!(
            bytes <= CAP_BYTES + 300 * 1024,
            "queue grew past its bound: {bytes} bytes"
        );
    }

    /// A genuine backlog still gets dropped — the cap is not disabled, only
    /// moved off the frame in hand.
    #[test]
    fn a_backlog_over_the_cap_is_still_dropped_and_resyncs() {
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
        let mut sink = StdoutSink {
            shared: Arc::clone(&shared),
            needs_resync: Arc::new(AtomicBool::new(false)),
            pending: Vec::new(),
            recycled: Vec::new(),
            high_water: 0,
        };
        // Small frames, no draining writer: the backlog builds past the cap.
        for _ in 0..40 {
            sink.write_all(&vec![b'x'; 8 * 1024]).expect("write");
            sink.flush().expect("flush");
        }
        assert!(
            sink.needs_resync.load(Ordering::Acquire),
            "a real backlog must still trip the cap"
        );
        let bytes = {
            let q = shared.queue.lock().expect("lock");
            q.bytes
        };
        assert!(
            bytes <= CAP_BYTES + 8 * 1024,
            "the backlog must be bounded: {bytes} bytes"
        );
    }

    /// The recycling actually ENGAGES for a large frame: the sink cycles a
    /// small set of allocations instead of minting one per frame.
    ///
    /// The gap the old test left. It asserted only that `spare_limit` grew,
    /// which it did — but the limit was sized from `chunk.len()` while both
    /// pooling sites gate on `buf.capacity()`. The frame is accumulated by
    /// many small writes (the renderer emits per cell), so the `Vec` grows by
    /// DOUBLING: a 300 KB frame lands in a 512 KiB allocation, measures
    /// 524288 against a limit of 307200, and is rejected. The sink then
    /// reallocated the frame buffer on every single frame while the pool sat
    /// empty — recycling that never engaged for exactly the frames it was
    /// widened to catch. Writing in pieces here is what reproduces that; one
    /// big `write_all` reserves exactly and hides the bug.
    #[test]
    fn a_large_frames_buffer_is_reused_rather_than_reallocated() {
        const FRAME: usize = 300 * 1024;
        const PIECE: usize = 4 * 1024;
        // A writer that discards, so buffers come straight back.
        struct Sink;
        impl Write for Sink {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (mut sink, handle) = spawn_writer_into(Sink);

        let piece = vec![b'#'; PIECE];
        // Did a frame-sized allocation ever reach the pool? Asserting on
        // POOL MEMBERSHIP, not on buffer addresses: freeing a 512 KiB block
        // and immediately asking for another usually returns the same
        // address, so pointer identity would pass even with recycling
        // completely disabled.
        let mut pooled_a_frame_sized_buffer = false;
        for _ in 0..6 {
            let mut written = 0;
            while written < FRAME {
                sink.write_all(&piece).expect("write");
                written += PIECE;
            }
            sink.flush().expect("flush");
            // Let the writer drain and hand the allocation back.
            std::thread::sleep(Duration::from_millis(20));
            let in_queue = {
                let q = sink
                    .shared
                    .queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                q.spare.iter().any(|b| b.capacity() >= FRAME)
            };
            pooled_a_frame_sized_buffer |= in_queue
                || sink.recycled.iter().any(|b| b.capacity() >= FRAME)
                || sink.pending.capacity() >= FRAME;
        }

        let limit = {
            let q = sink
                .shared
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            q.spare_limit
        };
        assert!(
            pooled_a_frame_sized_buffer,
            "a frame-sized buffer must survive in the pool; with the limit \
             sized from `len` ({limit}) every candidate measured its doubled \
             CAPACITY and was rejected, so the sink reallocated every frame"
        );
        assert!(
            limit >= FRAME,
            "the limit must cover the allocation a {FRAME}-byte frame really \
             occupies, not just its length; limit={limit}"
        );
        handle.shutdown_and_join();
    }

    /// The recycling limit follows the frames the terminal actually produces.
    /// A flat 64 KiB ceiling meant the pool disengaged at exactly the frame
    /// size where an allocation per frame costs most.
    #[test]
    fn the_spare_limit_grows_to_the_frames_actually_shipped() {
        let (mut sink, handle) = spawn_stdout_writer();
        sink.write_all(&vec![b'#'; 200 * 1024]).expect("write");
        sink.flush().expect("flush");
        let limit = {
            let q = sink
                .shared
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            q.spare_limit
        };
        assert!(
            limit >= 200 * 1024,
            "a 200 KiB frame must raise the recycling limit above the old \
             flat 64 KiB ceiling; limit={limit}"
        );
        assert!(limit <= SPARE_MAX_BYTES, "and stay bounded; limit={limit}");
        handle.shutdown_and_join();
    }

    #[test]
    fn empty_flush_is_a_noop() {
        let (mut sink, handle) = spawn_stdout_writer();
        sink.flush().expect("flush"); // no pending bytes
        assert!(!sink.needs_resync.load(Ordering::Acquire));
        handle.shutdown_and_join();
    }
}
