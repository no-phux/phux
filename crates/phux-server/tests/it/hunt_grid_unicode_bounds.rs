//! Regression coverage for the grid/scrollback/extract subsystem under
//! adversarial Unicode and history-bound inputs (wave-hunt/grid-extract):
//! ZWJ emoji, combining marks mid-cluster, scrollback overflow bounds, and
//! full-row / shrink-resize churn through the per-consumer reference diff.
#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::print_stderr, reason = "probe diagnostics on failure")]

use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions};
use phux_server::grid::{SCROLLBACK_ALL, SnapshotSynthesizer};
use phux_server::search::{Scope, SearchOptions, search_oneshot};

fn fresh(cols: u16, rows: u16, scrollback: usize) -> GhosttyTerminal<'static, 'static> {
    GhosttyTerminal::new(TerminalOptions {
        cols,
        rows,
        max_scrollback: scrollback,
    })
    .unwrap()
}

const fn vp() -> SearchOptions {
    SearchOptions {
        case_insensitive: false,
        include_viewport: true,
    }
}

// Probe 1: ZWJ emoji family (multi-codepoint single grapheme) extraction.
#[test]
fn probe_zwj_emoji_extract() {
    let mut t = fresh(20, 3, 100);
    // Family emoji: 👨‍👩‍👧 is multiple codepoints joined by ZWJ, one grapheme,
    // width 2. Followed by ASCII "END".
    t.vt_write("👨‍👩‍👧END".as_bytes());
    let synth = SnapshotSynthesizer::new().unwrap();
    let screen = synth.screen_state(&t, 0).unwrap();
    eprintln!("row0 = {:?}", screen.lines[0]);
    let hits = search_oneshot(&t, "END", Scope::AllHistory, vp()).unwrap();
    eprintln!("hits = {hits:?}");
    for m in &hits {
        let text = phux_server::extract::extract_match(&t, *m).unwrap();
        eprintln!("extract = {text:?}");
        assert_eq!(text, "END");
    }
}

// Probe 2: history-limit boundary — write far more than max_scrollback.
#[test]
fn probe_scrollback_overflow_bounds() {
    let mut t = fresh(10, 2, 5);
    for i in 0..100u32 {
        t.vt_write(format!("L{i}\r\n").as_bytes());
    }
    let total = t.scrollback_rows().unwrap();
    eprintln!("scrollback_rows after 100 lines, max=5: {total}");
    let synth = SnapshotSynthesizer::new().unwrap();
    let screen = synth
        .screen_state_with_scrollback(&t, 0, Some(SCROLLBACK_ALL), false)
        .unwrap();
    eprintln!("scrollback len = {}", screen.scrollback.len());
    // phux must project exactly what libghostty retains (max_scrollback is a
    // libghostty-internal budget, not a hard row cap), never more.
    assert_eq!(
        screen.scrollback.len(),
        total,
        "scrollback projection must equal scrollback_rows, never over-read",
    );
    // Request way more than exists.
    let screen2 = synth
        .screen_state_with_scrollback(&t, 0, Some(u32::MAX), false)
        .unwrap();
    assert_eq!(
        screen2.scrollback.len(),
        screen.scrollback.len(),
        "u32::MAX request clamps to available",
    );
}

// Probe 3: combining-mark cluster where a needle lands mid-cluster.
#[test]
fn probe_combining_mark_midcluster() {
    let mut t = fresh(20, 2, 100);
    // "e" + combining acute (U+0301) is one grapheme "é". Then "xy".
    t.vt_write("e\u{301}xy".as_bytes());
    let synth = SnapshotSynthesizer::new().unwrap();
    let screen = synth.screen_state(&t, 0).unwrap();
    eprintln!(
        "row0 = {:?} chars={:?}",
        screen.lines[0],
        screen.lines[0].chars().count()
    );
    // Search for "xy" — should be at char offset 2 (e, combining, then x).
    let hits = search_oneshot(&t, "xy", Scope::AllHistory, vp()).unwrap();
    eprintln!("hits = {hits:?}");
    for m in &hits {
        let text = phux_server::extract::extract_match(&t, *m).unwrap();
        eprintln!("extract = {text:?}");
        assert_eq!(text, "xy");
    }
}

// Probe 4: synthesize_against_reference under full-row churn, many rows.
#[test]
fn probe_reference_diff_full_churn() {
    use phux_server::grid::ConsumerReference;
    let mut t = fresh(80, 50, 200);
    let mut synth = SnapshotSynthesizer::new().unwrap();
    let mut reference = ConsumerReference::new();
    synth.prime_reference(&t, &mut reference).unwrap();
    // Churn every row.
    for row in 0..50u16 {
        t.vt_write(format!("\x1b[{};1Hrow-content-{row}", row + 1).as_bytes());
    }
    let diff = synth
        .synthesize_against_reference(&t, &mut reference)
        .unwrap();
    eprintln!("diff bytes = {}", diff.bytes.len());
    assert!(!diff.bytes.is_empty());
    // A second call with no change must be empty (emit-once).
    let diff2 = synth
        .synthesize_against_reference(&t, &mut reference)
        .unwrap();
    assert!(diff2.bytes.is_empty(), "no change -> empty diff");
}

// Probe 5: reference diff across a shrink resize between frames.
#[test]
fn probe_reference_diff_shrink_resize() {
    use phux_server::grid::ConsumerReference;
    let mut t = fresh(80, 24, 200);
    t.vt_write(b"line A\r\nline B\r\nline C");
    let mut synth = SnapshotSynthesizer::new().unwrap();
    let mut reference = ConsumerReference::new();
    synth.prime_reference(&t, &mut reference).unwrap();
    // Shrink dramatically.
    t.resize(10, 3, 8, 16).unwrap();
    let diff = synth
        .synthesize_against_reference(&t, &mut reference)
        .unwrap();
    eprintln!(
        "after shrink: dims=({},{}) bytes={}",
        diff.cols,
        diff.rows,
        diff.bytes.len()
    );
    assert_eq!((diff.cols, diff.rows), (10, 3));
}
