//! phux-51n6.3 / ADR-0042 — state-diff output-mode convergence + pacing.
//!
//! Proves the two properties the negotiated `OutputMode::StateSync` emitter
//! must guarantee (ADR-0018 / ADR-0042):
//!
//! 1. **Convergence.** A consumer served the per-consumer synthesized deltas
//!    (the `synthesize_against_reference` reference-grid path the tick uses)
//!    ends on a grid byte-identical to a pass-through consumer fed the raw PTY
//!    bytes — across SGR runs, cursor moves, scroll regions, alt-screen, and
//!    wide (CJK/emoji) glyphs. This is the "a state-diff consumer and a
//!    pass-through consumer viewing the same terminal see identical final
//!    screens" contract.
//!
//! 2. **Pacing / coalescence.** A runaway app that repaints thousands of
//!    intermediate frames between two ticks produces exactly one bounded delta
//!    per tick, sized by the visible state change rather than the byte volume —
//!    so the client's re-parse/re-render RATE is bounded — while still landing
//!    the correct final state.
//!
//! libghostty types are `!Send + !Sync`; the whole test runs on the test
//! thread with no tokio runtime — this exercises the pure synthesizer path.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use libghostty_vt::render::{CellIterator, RenderState, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions};
use phux_server::grid::{ConsumerReference, SnapshotSynthesizer};

fn fresh(cols: u16, rows: u16) -> GhosttyTerminal<'static, 'static> {
    GhosttyTerminal::new(TerminalOptions {
        cols,
        rows,
        max_scrollback: 100,
    })
    .expect("Terminal::new")
}

/// Right-trimmed viewport text rows, wide-cell tails skipped.
fn render_grid(t: &GhosttyTerminal<'_, '_>) -> Vec<String> {
    let mut rs = RenderState::new().expect("RenderState::new");
    let snap = rs.update(t).expect("update");
    let rows_n = snap.rows().expect("rows");
    let mut rows = RowIterator::new().expect("RowIterator::new");
    let mut cells = CellIterator::new().expect("CellIterator::new");
    let mut row_iter = rows.update(&snap).expect("row update");
    let mut grid: Vec<String> = Vec::with_capacity(usize::from(rows_n));
    let mut i: u16 = 0;
    while let Some(row) = row_iter.next() {
        if i >= rows_n {
            break;
        }
        let mut line = String::new();
        let mut cell_iter = cells.update(row).expect("cell update");
        while let Some(cell) = cell_iter.next() {
            if matches!(
                cell.raw_cell().expect("rc").wide().expect("wide"),
                CellWide::SpacerTail
            ) {
                continue;
            }
            let g = cell.graphemes().expect("graphemes");
            if g.is_empty() {
                line.push(' ');
            } else {
                line.extend(g);
            }
        }
        grid.push(line.trim_end().to_owned());
        i += 1;
    }
    grid
}

/// A full-grid snapshot of `t` (SGR + text + cursor + modes), used to assert
/// grid equivalence *including styling* between two mirrors: byte-identical
/// snapshots ⇒ equivalent grids.
fn full_snapshot(t: &GhosttyTerminal<'_, '_>) -> Vec<u8> {
    SnapshotSynthesizer::new()
        .expect("synth")
        .synthesize(t)
        .expect("synthesize")
        .bytes
}

/// Drive `chunks` through a canonical Terminal, feeding one mirror the raw
/// bytes (pass-through, ADR-0013 degenerate case) and another the per-tick
/// synthesized reference-grid deltas (state-sync). Assert all three grids
/// converge — text AND full-snapshot (style-inclusive).
fn assert_converges(cols: u16, rows: u16, chunks: &[&[u8]]) {
    let mut canonical = fresh(cols, rows);
    let mut synth = SnapshotSynthesizer::new().expect("synth");
    let mut reference = ConsumerReference::new();
    // Prime the reference to the (empty) canonical — the point a fresh mirror
    // and the consumer's TERMINAL_SNAPSHOT both start from.
    synth
        .prime_reference(&canonical, &mut reference)
        .expect("prime");

    let mut statesync_mirror = fresh(cols, rows);
    let mut passthrough_mirror = fresh(cols, rows);

    for chunk in chunks {
        canonical.vt_write(chunk);
        passthrough_mirror.vt_write(chunk);
        // One tick per chunk: synthesize the minimum-VT transition from the
        // consumer's reference to the current canonical, apply to the mirror.
        let delta = synth
            .synthesize_against_reference(&canonical, &mut reference)
            .expect("synthesize_against_reference")
            .bytes;
        statesync_mirror.vt_write(&delta);
    }

    let canon = render_grid(&canonical);
    let ss = render_grid(&statesync_mirror);
    let pt = render_grid(&passthrough_mirror);
    assert_eq!(
        ss, pt,
        "state-sync and pass-through mirrors must render identical grids;\n\
         state-sync  = {ss:?}\npass-through = {pt:?}",
    );
    assert_eq!(
        ss, canon,
        "state-sync mirror must match canonical;\nmirror = {ss:?}\ncanon = {canon:?}",
    );
    // Style-inclusive equivalence: the two mirrors' full snapshots agree.
    assert_eq!(
        full_snapshot(&statesync_mirror),
        full_snapshot(&passthrough_mirror),
        "state-sync and pass-through grids must be equivalent including SGR/cursor/modes",
    );
}

#[test]
fn converges_plain_text_and_newlines() {
    assert_converges(20, 5, &[b"first line", b"\r\nsecond line", b"\r\nthird"]);
}

#[test]
fn converges_sgr_color_runs() {
    assert_converges(
        30,
        4,
        &[
            b"\x1b[31mRED\x1b[0m normal ",
            b"\x1b[1;32mBOLDGREEN\x1b[0m",
            b"\r\n\x1b[38;2;10;20;30mtruecolor\x1b[0m tail",
        ],
    );
}

#[test]
fn converges_cursor_moves_and_overwrite() {
    assert_converges(
        20,
        5,
        &[
            b"\x1b[2;3Habc",    // CUP then write
            b"\x1b[1;1HTOP",    // jump home, overwrite
            b"\x1b[5;1Hbottom", // last row
            b"\rXX",            // CR then overwrite start of current row
        ],
    );
}

#[test]
fn converges_scroll_region() {
    // Set a scroll region rows 2..4, fill it, then push newlines so the region
    // scrolls independently of rows outside it.
    assert_converges(
        16,
        6,
        &[
            b"top-fixed\r\n",
            b"\x1b[2;4r", // DECSTBM: scroll region rows 2-4
            b"\x1b[2;1Hline-a\r\nline-b\r\nline-c",
            b"\r\nline-d", // forces the region to scroll
            b"\r\nline-e",
        ],
    );
}

#[test]
fn converges_alt_screen_toggle() {
    assert_converges(
        20,
        5,
        &[
            b"primary content",
            b"\x1b[?1049h", // enter alt screen (clears it)
            b"alt-screen body",
            b"\r\nmore alt",
            b"\x1b[?1049l", // leave alt screen — primary must return
        ],
    );
}

#[test]
fn converges_wide_chars() {
    // CJK + emoji occupy two columns each; the synthesizer must reproduce the
    // wide-cell layout so the mirror grid lines up column-for-column.
    assert_converges(
        20,
        4,
        &[
            "日本語テスト".as_bytes(),
            b"\r\n",
            "emoji \u{1f600}\u{1f680} end".as_bytes(),
        ],
    );
}

#[test]
fn converges_interleaved_feature_stress() {
    // Everything at once, in one run: SGR + cursor + wide + alt-screen round
    // trip + scroll region.
    assert_converges(
        24,
        6,
        &[
            b"\x1b[33mstatus\x1b[0m ",
            "\u{1f4bb} 日本".as_bytes(),
            b"\x1b[3;1H\x1b[4;1r",
            b"\r\nscroll-1\r\nscroll-2\r\nscroll-3",
            b"\x1b[?1049h",
            b"\x1b[1;1Halt \x1b[31mred\x1b[0m",
            b"\x1b[?1049l",
            b"\x1b[6;1Hfinal-row",
        ],
    );
}

/// Pacing / coalescence: a runaway app repaints many intermediate frames
/// between two ticks. The state-diff tick coalesces them into ONE delta sized
/// by the visible state change, not the byte volume — bounding the client's
/// re-parse rate — while still converging on the final state.
#[test]
fn runaway_output_is_coalesced_and_final_state_correct() {
    let mut canonical = fresh(40, 5);
    let mut synth = SnapshotSynthesizer::new().expect("synth");
    let mut reference = ConsumerReference::new();
    synth
        .prime_reference(&canonical, &mut reference)
        .expect("prime");
    let mut mirror = fresh(40, 5);

    // Simulate a spinner / progress bar repainting row 0 thousands of times
    // between the two ticks the consumer actually gets scheduled for.
    let frames = 5000_u32;
    let mut raw_volume = 0_usize;
    for i in 0..frames {
        let paint = format!("\x1b[1;1Hframe {i:06} \x1b[7m{}\x1b[0m", "#".repeat(10));
        raw_volume += paint.len();
        canonical.vt_write(paint.as_bytes());
    }
    // Final visible state.
    let done = b"\x1b[1;1HDONE                    \x1b[2;1Hresult: ok";
    raw_volume += done.len();
    canonical.vt_write(done);

    // ONE tick: a single coalesced delta covers all 5000 intermediate frames.
    let delta = synth
        .synthesize_against_reference(&canonical, &mut reference)
        .expect("synthesize")
        .bytes;
    mirror.vt_write(&delta);

    // Correctness: the final state landed.
    assert_eq!(
        render_grid(&mirror),
        render_grid(&canonical),
        "coalesced delta must converge the mirror to the final state",
    );
    assert!(render_grid(&mirror)[0].starts_with("DONE"));

    // Rate-bounding: the single delta is a tiny fraction of the raw byte volume
    // the runaway app produced — the client re-parses the coalesced delta, not
    // every intermediate frame.
    assert!(
        delta.len() * 20 < raw_volume,
        "coalesced delta ({} bytes) must be far smaller than the runaway raw \
         volume ({} bytes) — that bound is the pacing win",
        delta.len(),
        raw_volume,
    );
}

/// Coalescence across ticks stays correct: driving the runaway in several
/// ticks (each coalescing a slice of the repaints) still converges, and each
/// per-tick delta stays bounded.
#[test]
fn runaway_across_multiple_ticks_stays_bounded_and_converges() {
    let mut canonical = fresh(40, 4);
    let mut synth = SnapshotSynthesizer::new().expect("synth");
    let mut reference = ConsumerReference::new();
    synth
        .prime_reference(&canonical, &mut reference)
        .expect("prime");
    let mut mirror = fresh(40, 4);

    for tick in 0..10 {
        // 500 intermediate repaints per tick.
        for i in 0..500 {
            let paint = format!("\x1b[1;1Htick {tick} frame {i:04}");
            canonical.vt_write(paint.as_bytes());
        }
        let delta = synth
            .synthesize_against_reference(&canonical, &mut reference)
            .expect("synthesize")
            .bytes;
        // Each per-tick delta is bounded by ~one repainted row, never the 500
        // intermediate frames.
        assert!(
            delta.len() < 400,
            "per-tick coalesced delta must stay row-bounded; got {} bytes",
            delta.len(),
        );
        mirror.vt_write(&delta);
    }

    assert_eq!(
        render_grid(&mirror),
        render_grid(&canonical),
        "multi-tick runaway must still converge",
    );
}
