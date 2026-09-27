//! Allocation gate for the per-consumer state-sync diff
//! (`SnapshotSynthesizer::synthesize_against_reference`) under full colored
//! churn. It once allocated a `Vec` per row and a `Vec<char>` per cell
//! (~2000 allocations per tick at 80x40); it now reuses scratch buffers
//! (~48/tick). Counts allocations, not wall time, but 200 ticks are ~110s of
//! CPU, so it lives in the stress lane.

#![allow(clippy::print_stderr, reason = "prints the measured figure for triage")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

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

/// Between the fixed (~48) and pre-fix (~2041) figures: trips if a per-row
/// or per-cell allocation comes back.
const MAX_ALLOCS_PER_TICK: usize = 250;

#[test]
#[ignore = "runs in the stress lane (`just stress`): ~110s of CPU-bound churn"]
fn synthesize_against_reference_alloc_bounded_under_full_churn() {
    let mut t = GhosttyTerminal::new(COLS, ROWS).expect("Terminal::new");
    t.set_scrollback_max_lines(Some(100)).expect("scrollback");
    let mut synth = SnapshotSynthesizer::new().expect("synth");
    let mut reference = ConsumerReference::new();
    synth
        .prime_reference(&t, &mut reference)
        .expect("prime_reference");

    // Warm up so scratch buffers reach steady capacity.
    for i in 0..2 {
        write_burst(&mut t, i);
        let _ = synth
            .synthesize_against_reference(&t, &mut reference)
            .expect("warmup");
    }

    let ticks: usize = 200;
    let start = ALLOCS.load(Ordering::Relaxed);
    for i in 0..ticks {
        write_burst(&mut t, i + 2);
        let diff = synth
            .synthesize_against_reference(&t, &mut reference)
            .expect("synth");
        assert!(!diff.bytes.is_empty(), "tick {i} measured a clean tick");
        std::hint::black_box(&diff.bytes);
    }
    let total = ALLOCS.load(Ordering::Relaxed) - start;
    eprintln!(
        "bursty-output: {total} allocs over {ticks} ticks ({}/tick)",
        total / ticks
    );
    assert!(
        total <= MAX_ALLOCS_PER_TICK * ticks,
        "{}/tick exceeds {MAX_ALLOCS_PER_TICK}: a per-row or per-cell allocation regressed",
        total / ticks,
    );
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
