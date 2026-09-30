//! Allocation and damage-proportionality gates for the per-consumer
//! state-sync diff (`SnapshotSynthesizer::synthesize_against_reference`).
//!
//! Full colored churn: it once allocated a `Vec` per row and a `Vec<char>`
//! per cell (~2000 allocations per tick at 80x40); it now reuses scratch
//! buffers, so a tick allocates only its diff. Only the synthesis call is
//! counted and timed, not the workload writes.
//!
//! One-row churn (phux-69pq.13): the tick re-renders only the rows the
//! engine reports dirty, so a one-row tick must cost a small fraction of an
//! all-rows tick. Before the incremental tick it cost ~70-80% (every row was
//! re-rendered regardless); the ratio gate trips if that comes back.

#![allow(clippy::print_stderr, reason = "prints the measured figure for triage")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use libghostty_vt::Terminal as GhosttyTerminal;
use phux_server::grid::{ConsumerReference, SnapshotSynthesizer};

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: delegates every operation to the system allocator unchanged; the
// only addition is a relaxed counter bump on alloc.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static A: Counting = Counting;

const COLS: u16 = 80;
const ROWS: u16 = 40;
const TICKS: usize = 200;

/// Far below the pre-fix (~2041) figure: trips if a per-row or per-cell
/// allocation comes back.
const MAX_ALLOCS_PER_TICK: usize = 250;

/// A one-row tick may cost at most this share of an all-rows tick. Measured
/// ~0.7-0.8 when every tick re-rendered every row.
const MAX_ONE_ROW_TIME_RATIO: f64 = 0.4;

#[test]
#[ignore = "runs in the stress lane (`just stress`) with the other perf gates"]
fn synthesize_against_reference_alloc_bounded_under_full_churn() {
    let full = Churn::new().measure(write_burst);
    eprintln!(
        "bursty-output: all {ROWS} rows changed: {} allocs/tick, {:?}/tick",
        full.allocs_per_tick, full.time_per_tick,
    );
    assert!(
        full.allocs_per_tick <= MAX_ALLOCS_PER_TICK,
        "{}/tick exceeds {MAX_ALLOCS_PER_TICK}: a per-row or per-cell allocation regressed",
        full.allocs_per_tick,
    );
}

#[test]
#[ignore = "runs in the stress lane (`just stress`) beside the full-churn gate"]
fn one_row_tick_costs_a_fraction_of_a_full_tick() {
    let full = Churn::new().measure(write_burst);
    let one = Churn::new().measure(write_one_row);
    #[allow(clippy::cast_precision_loss, reason = "a ratio of two durations")]
    let ratio = one.time_per_tick.as_nanos() as f64 / full.time_per_tick.as_nanos() as f64;
    eprintln!(
        "bursty-output: 1 of {ROWS} rows changed: {} allocs/tick, {:?}/tick \
         ({ratio:.2}x the all-rows tick: {} allocs/tick, {:?}/tick)",
        one.allocs_per_tick, one.time_per_tick, full.allocs_per_tick, full.time_per_tick,
    );
    assert!(
        one.allocs_per_tick <= full.allocs_per_tick,
        "a one-row tick allocated more than an all-rows tick",
    );
    assert!(
        ratio <= MAX_ONE_ROW_TIME_RATIO,
        "a one-row tick took {ratio:.2}x an all-rows tick (max {MAX_ONE_ROW_TIME_RATIO}): \
         the tick is re-rendering unchanged rows",
    );
}

/// One terminal, synthesizer, and primed consumer reference.
struct Churn {
    terminal: GhosttyTerminal<'static, 'static>,
    synth: SnapshotSynthesizer<'static>,
    reference: ConsumerReference,
}

/// Per-tick cost of the synthesis call alone (the workload write excluded).
struct Cost {
    allocs_per_tick: usize,
    time_per_tick: Duration,
}

impl Churn {
    fn new() -> Self {
        let mut terminal = GhosttyTerminal::new(COLS, ROWS).expect("Terminal::new");
        terminal
            .set_scrollback_max_lines(Some(100))
            .expect("scrollback");
        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let mut reference = ConsumerReference::new();
        synth
            .prime_reference(&terminal, &mut reference)
            .expect("prime_reference");
        let mut churn = Self {
            terminal,
            synth,
            reference,
        };
        // Paint every row, then warm scratch buffers to steady capacity.
        for i in 0..2 {
            write_burst(&mut churn.terminal, i);
            churn.tick();
        }
        churn
    }

    fn tick(&mut self) -> Vec<u8> {
        self.synth
            .synthesize_against_reference(&self.terminal, &mut self.reference)
            .expect("synth")
            .bytes
    }

    fn measure(mut self, write: fn(&mut GhosttyTerminal<'_, '_>, usize)) -> Cost {
        let mut allocs = 0;
        let mut time = Duration::ZERO;
        for i in 0..TICKS {
            write(&mut self.terminal, i + 2);
            let start_allocs = ALLOCS.load(Ordering::Relaxed);
            let start = Instant::now();
            let bytes = self.tick();
            time += start.elapsed();
            allocs += ALLOCS.load(Ordering::Relaxed) - start_allocs;
            assert!(!bytes.is_empty(), "tick {i} measured a clean tick");
            std::hint::black_box(&bytes);
        }
        Cost {
            allocs_per_tick: allocs / TICKS,
            time_per_tick: time / u32::try_from(TICKS).expect("tick count"),
        }
    }
}

/// Rewrite every row with a distinct 256-color foreground and a per-tick marker.
fn write_burst(t: &mut GhosttyTerminal<'_, '_>, iter: usize) {
    t.vt_write(b"\x1b[H");
    for r in 0..ROWS {
        let fg = 16 + (u32::from(r) % 200);
        t.vt_write(format!("\x1b[38;5;{fg}mrow {r:02} g{iter} ").as_bytes());
        for _ in 0..6 {
            t.vt_write(b"colored-chunk ");
        }
        if r + 1 < ROWS {
            t.vt_write(b"\r\n");
        }
    }
}

/// Rewrite one mid-grid row the same way, leaving the cursor on it.
fn write_one_row(t: &mut GhosttyTerminal<'_, '_>, iter: usize) {
    let fg = 16 + iter % 200;
    t.vt_write(format!("\x1b[{};1H\x1b[38;5;{fg}mrow g{iter} ", ROWS / 2).as_bytes());
    for _ in 0..6 {
        t.vt_write(b"colored-chunk ");
    }
}
