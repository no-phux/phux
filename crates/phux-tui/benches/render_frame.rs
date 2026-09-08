//! Cell-emission gate for the client's pane renderer (`attach::render`).
//!
//! This is the tightest loop in the product: grid cells -> ANSI bytes on
//! stdout. It measures the two shapes that matter, against the shared
//! `benchmarks/support.rs` corpora:
//!
//! * **full-dirty** — every row repainted (`render_at_full`), which is what a
//!   scroll (`Dirty::Full`) and every `paint_full_frame` pay.
//! * **one-row-dirty** — the steady-state incremental paint: one row written,
//!   `render_at` walks the grid and emits only that row.
//! * **clean** — the most FREQUENT call of the three: a pane that produced no
//!   output while some other pane, or the chrome, forced a repaint. It must
//!   cost zero bytes and zero flushes.
//!
//! Three numbers per case: wall time per frame (p50/p90 over
//! `MEASURED_SAMPLES`), bytes emitted per frame, and heap allocations per
//! frame. The allocation count is the point: the pre-`phux-l96p.2` cell loop
//! allocated a `Vec<char>` per non-empty cell, so a full-dirty 200x60 frame
//! cost ~10k malloc/free pairs before a single byte reached the terminal.
//!
//! Run: `cargo bench -p phux-client --features testkit --bench render_frame`.

#![allow(
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::print_stdout,
    missing_docs,
    reason = "measurement-reporting benchmark binary"
)]

#[path = "../../../benchmarks/support.rs"]
mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::io::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions};
use phux_tui::attach::render::{ReplicaWalk, TerminalRenderer};
use support::{Corpus, MEASURED_SAMPLES, WARMUP_SAMPLES, deterministic_line, percentile};

/// Allocation counter.
///
/// A pass-through to the system allocator plus one relaxed increment, so the
/// timing numbers this binary also reports stay representative (a full
/// profiler such as `dhat` records backtraces and would dominate them).
struct CountingAlloc;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards its arguments unchanged to `System`, which is
// a correct `GlobalAlloc`; the only added work is a relaxed counter bump that
// touches no allocator state. The safety contract of each method is therefore
// exactly `System`'s, and the caller's obligations are passed straight
// through.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

fn allocs() -> usize {
    ALLOCS.load(Ordering::Relaxed)
}

/// A stdout stand-in that keeps its buffer (so the sink itself never
/// allocates on the measured path) and counts `flush` calls — the
/// "one flush per frame" half of the gate.
#[derive(Debug, Default)]
struct CountingSink {
    buf: Vec<u8>,
    flushes: usize,
}

impl CountingSink {
    fn reset(&mut self) {
        self.buf.clear();
        self.flushes = 0;
    }
}

impl Write for CountingSink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.buf.extend_from_slice(data);
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

/// Build the corpus grid the renderer walks.
///
/// Mirrors `phux-server/benches/server_measure.rs::build_terminal` so both
/// ends of the wire are measured against the same cells.
fn build_terminal(corpus: Corpus) -> GhosttyTerminal<'static, 'static> {
    let (cols, rows) = corpus.geometry();
    let mut terminal = GhosttyTerminal::new(TerminalOptions {
        cols,
        rows,
        max_scrollback: corpus.history_lines().max(1_000),
    })
    .expect("benchmark terminal");

    match corpus {
        Corpus::Shell80x24 => {
            terminal.vt_write(b"$ printf 'ready\\n'\r\nready\r\n$ ");
            terminal.vt_write(b"\x1b[1;32mbranch\x1b[0m feat/negotiated-libghostty-codec\r\n");
            terminal
                .vt_write("wide: \u{6771}\u{4eac} \u{1f980} combining: e\u{301}\r\n".as_bytes());
        }
        Corpus::Tui200x60 | Corpus::Unicode50k => {
            terminal.vt_write(b"\x1b[?1049h\x1b[2J\x1b[H");
            for row in 0..rows {
                let color = 16 + (u32::from(row) * 37 % 216);
                let line = format!(
                    "\x1b[{};1H\x1b[38;5;{}m{:03} {:<170}\x1b[0m",
                    row + 1,
                    color,
                    row,
                    deterministic_line(usize::from(row)).trim_end(),
                );
                terminal.vt_write(line.as_bytes());
            }
            terminal.vt_write(b"\x1b[30;70H\x1b[7m ACTIVE \x1b[0m");
        }
    }
    terminal
}

#[derive(Debug)]
struct Measurement {
    p50: Duration,
    p90: Duration,
    bytes: usize,
    allocs: usize,
    flushes: usize,
}

fn report(case: &str, corpus: Corpus, m: &Measurement) {
    println!(
        "{case:<16} {label:<12} p50={p50:>9.0}ns p90={p90:>9.0}ns bytes/frame={bytes:>7} \
         allocs/frame={allocs:>6} flushes/frame={flushes}",
        label = corpus.label(),
        p50 = m.p50.as_nanos() as f64,
        p90 = m.p90.as_nanos() as f64,
        bytes = m.bytes,
        allocs = m.allocs,
        flushes = m.flushes,
    );
}

/// Full-dirty repaint: `render_at_full` forces every row, the `Dirty::Full`
/// cost a scroll pays.
fn measure_full(corpus: Corpus) -> Measurement {
    let terminal = build_terminal(corpus);
    let (cols, rows) = corpus.geometry();
    let mut renderer = TerminalRenderer::new().expect("renderer");
    let mut sink = CountingSink::default();

    for _ in 0..WARMUP_SAMPLES {
        sink.reset();
        let _ = renderer.render_at_full(
            ReplicaWalk::for_test(&terminal),
            &mut sink,
            (0, 0),
            (cols, rows),
        );
    }

    // One clean, fully-warm frame for the byte/alloc/flush counts.
    sink.reset();
    let before = allocs();
    let _ = renderer.render_at_full(
        ReplicaWalk::for_test(&terminal),
        &mut sink,
        (0, 0),
        (cols, rows),
    );
    let per_frame_allocs = allocs() - before;
    let bytes = sink.buf.len();
    let flushes = sink.flushes;

    let mut samples = Vec::with_capacity(MEASURED_SAMPLES);
    for _ in 0..MEASURED_SAMPLES {
        sink.reset();
        let start = Instant::now();
        let _ = renderer.render_at_full(
            ReplicaWalk::for_test(black_box(&terminal)),
            &mut sink,
            (0, 0),
            (cols, rows),
        );
        samples.push(start.elapsed());
        black_box(sink.buf.len());
    }

    Measurement {
        p50: percentile(&mut samples, 50),
        p90: percentile(&mut samples, 90),
        bytes,
        allocs: per_frame_allocs,
        flushes,
    }
}

/// Steady-state incremental paint: dirty exactly one row, then let
/// `render_at`'s per-row dirty tracking emit only that row.
fn measure_one_row(corpus: Corpus) -> Measurement {
    let mut terminal = build_terminal(corpus);
    let (cols, rows) = corpus.geometry();
    let target_row = rows / 2;
    let mut renderer = TerminalRenderer::new().expect("renderer");
    let mut sink = CountingSink::default();

    // Alternate two payloads so every iteration is a real content change.
    let payloads: [String; 2] = [
        format!("\x1b[{};1H\x1b[38;5;70mrow A \x1b[0m", target_row + 1),
        format!("\x1b[{};1H\x1b[38;5;71mrow B \x1b[0m", target_row + 1),
    ];

    // Warm up, and drain the initial `Dirty::Full` the first walk reports.
    for i in 0..WARMUP_SAMPLES {
        terminal.vt_write(payloads[i % 2].as_bytes());
        sink.reset();
        let _ = renderer.render_at(
            ReplicaWalk::for_test(&terminal),
            &mut sink,
            (0, 0),
            (cols, rows),
        );
    }

    terminal.vt_write(payloads[0].as_bytes());
    sink.reset();
    let before = allocs();
    let _ = renderer.render_at(
        ReplicaWalk::for_test(&terminal),
        &mut sink,
        (0, 0),
        (cols, rows),
    );
    let per_frame_allocs = allocs() - before;
    let bytes = sink.buf.len();
    let flushes = sink.flushes;

    let mut samples = Vec::with_capacity(MEASURED_SAMPLES);
    for i in 0..MEASURED_SAMPLES {
        terminal.vt_write(payloads[i % 2].as_bytes());
        sink.reset();
        let start = Instant::now();
        let _ = renderer.render_at(
            ReplicaWalk::for_test(black_box(&terminal)),
            &mut sink,
            (0, 0),
            (cols, rows),
        );
        samples.push(start.elapsed());
        black_box(sink.buf.len());
    }

    Measurement {
        p50: percentile(&mut samples, 50),
        p90: percentile(&mut samples, 90),
        bytes,
        allocs: per_frame_allocs,
        flushes,
    }
}

/// The idle pane: nothing changed since the last paint, so the walk must find
/// no dirty rows and emit nothing at all.
fn measure_clean(corpus: Corpus) -> Measurement {
    let terminal = build_terminal(corpus);
    let (cols, rows) = corpus.geometry();
    let mut renderer = TerminalRenderer::new().expect("renderer");
    let mut sink = CountingSink::default();

    for _ in 0..WARMUP_SAMPLES {
        sink.reset();
        let _ = renderer.render_at(
            ReplicaWalk::for_test(&terminal),
            &mut sink,
            (0, 0),
            (cols, rows),
        );
    }

    sink.reset();
    let before = allocs();
    let _ = renderer.render_at(
        ReplicaWalk::for_test(&terminal),
        &mut sink,
        (0, 0),
        (cols, rows),
    );
    let per_frame_allocs = allocs() - before;
    let bytes = sink.buf.len();
    let flushes = sink.flushes;

    let mut samples = Vec::with_capacity(MEASURED_SAMPLES);
    for _ in 0..MEASURED_SAMPLES {
        sink.reset();
        let start = Instant::now();
        let _ = renderer.render_at(
            ReplicaWalk::for_test(black_box(&terminal)),
            &mut sink,
            (0, 0),
            (cols, rows),
        );
        samples.push(start.elapsed());
        black_box(sink.buf.len());
    }

    Measurement {
        p50: percentile(&mut samples, 50),
        p90: percentile(&mut samples, 90),
        bytes,
        allocs: per_frame_allocs,
        flushes,
    }
}

fn main() {
    println!("phux-client attach::render cell-emission gate");
    for corpus in [Corpus::Shell80x24, Corpus::Tui200x60] {
        report("full-dirty", corpus, &measure_full(corpus));
        report("one-row-dirty", corpus, &measure_one_row(corpus));
        report("clean", corpus, &measure_clean(corpus));
    }
}
