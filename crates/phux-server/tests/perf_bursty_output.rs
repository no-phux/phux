//! Bursty-output allocation gate (wave-fix/perf-bursty-output).
//!
//! Heavy, rapidly-repainting COLORED terminal output (zsh completion
//! menus, syntax-highlighted output, anything that churns every row with
//! lots of SGR) used to stutter the attached session. The dominant
//! server-side cost was the per-consumer state-sync diff
//! ([`SnapshotSynthesizer::synthesize_against_reference`]): it allocated a
//! fresh `Vec<u8>` per row every tick AND, inside the cell walk,
//! `CellIteration::graphemes()` allocated a `Vec<char>` for *every cell*
//! (`vec!['\0'; len]`). On an 80x40 grid under full churn that was ~2000
//! heap allocations per tick — pure churn the GC-less hot path paid on
//! every 33 Hz tick, per consumer.
//!
//! The fix renders each row into a reused scratch buffer (swapped into the
//! reference's stored body so the displaced allocation becomes the next
//! row's scratch) and reads grapheme clusters into a stack buffer via
//! `graphemes_len` + `graphemes_buf`. Steady-state churn now allocates
//! only the per-frame output `Vec` plus a small, bounded constant.
//!
//! This test asserts a hard per-tick allocation ceiling well below the
//! pre-fix figure so a regression (e.g. reintroducing `graphemes()` or a
//! per-row `Vec`) trips CI. It is a coarse architectural gate, not a
//! microbenchmark: the bound has generous headroom over the measured
//! steady-state (~48 allocs/tick for 40 rows) so it is not flaky across
//! allocator/std differences.
//!
//! It runs `#[ignore]`d out of the default pool, in the stress lane
//! (`just stress`, post-merge + nightly): 200 full-churn ticks are ~110s
//! of pure CPU on a hosted runner — the single longest test in the PR
//! unit pool before it was evicted. It is not starvation-sensitive (the
//! gate counts allocations, not wall time), so the stress lane's shared
//! runner is a safe home.

#![allow(
    clippy::print_stderr,
    reason = "perf gate prints the measured allocation figure for triage on failure"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Allocation-counting global allocator. Counts every `alloc` call across
/// the whole test process; we sample it around the hot loop so the count
/// reflects only the synthesis work.
struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: delegates every operation to the system allocator unchanged;
// the only added behaviour is a relaxed counter bump on alloc, which has
// no bearing on allocation correctness.
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

use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions};
use phux_server::grid::{ConsumerReference, SnapshotSynthesizer};

const COLS: u16 = 80;
const ROWS: u16 = 40;

/// Allocations per tick must stay well under this ceiling under full
/// colored churn. Pre-fix the figure was ~2041/tick; post-fix ~48/tick.
/// The ceiling sits between, leaving generous headroom over steady-state
/// while still tripping if either per-row `Vec` allocation or per-cell
/// `graphemes()` allocation is reintroduced (~ROWS or ~ROWS*COLS allocs).
const MAX_ALLOCS_PER_TICK: usize = 250;

/// Drive `ROWS` rows of distinctly-colored, fully-churning content through
/// the per-consumer diff and assert the per-tick allocation count stays
/// bounded. This is the headless stand-in for the interactive repro
/// (zsh completion menu / syntax-highlighted scroll).
#[test]
#[ignore = "runs in the stress lane (`just stress`): ~110s of CPU-bound churn, too heavy for the PR unit pool"]
fn synthesize_against_reference_alloc_bounded_under_full_churn() {
    let mut t = GhosttyTerminal::new(TerminalOptions {
        cols: COLS,
        rows: ROWS,
        max_scrollback: 100,
    })
    .expect("Terminal::new");
    let mut synth = SnapshotSynthesizer::new().expect("synth");
    let mut reference = ConsumerReference::new();
    synth
        .prime_reference(&t, &mut reference)
        .expect("prime_reference");

    // Warm up: a couple of bursts so the scratch buffers + reference row
    // bodies reach steady capacity and stop growing.
    for i in 0..2usize {
        write_burst(&mut t, i);
        let _ = synth
            .synthesize_against_reference(&t, &mut reference)
            .expect("warmup synth");
    }

    let ticks: usize = 200;
    let start = ALLOCS.load(Ordering::Relaxed);
    let mut total_bytes = 0usize;
    for i in 0..ticks {
        write_burst(&mut t, i + 2);
        let diff = synth
            .synthesize_against_reference(&t, &mut reference)
            .expect("synth");
        // Every burst rewrites every row, so the diff must be non-empty
        // (this guards against the test silently measuring a clean tick).
        assert!(
            !diff.bytes.is_empty(),
            "full-churn tick {i} produced an empty diff",
        );
        total_bytes += diff.bytes.len();
        std::hint::black_box(&diff.bytes);
    }
    let total = ALLOCS.load(Ordering::Relaxed) - start;
    // Integer per-tick figures (whole + tenths) so the diagnostic avoids a
    // lossy `usize as f64` cast under the workspace's deny-warnings gate.
    let per_tick_whole = total / ticks;
    let per_tick_tenths = (total * 10 / ticks) % 10;
    eprintln!(
        "bursty-output: {total} allocs over {ticks} full-churn ticks = \
         {per_tick_whole}.{per_tick_tenths}/tick ({ROWS} rows, {COLS} cols); \
         diff bytes/tick = {}",
        total_bytes / ticks,
    );

    assert!(
        total <= MAX_ALLOCS_PER_TICK * ticks,
        "per-tick allocations {per_tick_whole}.{per_tick_tenths} exceed ceiling \
         {MAX_ALLOCS_PER_TICK}; a per-row Vec or per-cell graphemes() allocation \
         likely regressed",
    );
}

/// Write a full screen of SGR-laden content: every row gets a distinct
/// 256-color foreground plus a per-iteration marker so the body differs
/// from the previous tick (forcing a full repaint diff every tick).
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
