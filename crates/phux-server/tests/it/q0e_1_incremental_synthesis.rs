//! `phux-q0e.1` — incremental synthesis path on `SnapshotSynthesizer`.
//!
//! Covers the four behaviors the ticket pins (per ADR-0018 and
//! `research/archive/2026-05-26-state-sync-algorithm.md` Dependencies §2):
//!
//! 1. **Clean roundtrip.** Take a baseline full synthesis. Without further
//!    `vt_write`, call `synthesize_incremental` — bytes must be empty.
//! 2. **Partial roundtrip.** Baseline, then `vt_write` new content on the
//!    canonical, then `synthesize_incremental`, then apply to a mirror that
//!    was already brought up to baseline. The mirror grid must match the
//!    canonical's grid.
//! 3. **Loss-tolerance invariant.** Two back-to-back
//!    `synthesize_incremental` calls without clearing dirty bits must
//!    *both* produce a non-empty diff that — applied independently to two
//!    fresh mirrors brought up to baseline — leaves both mirrors in the
//!    canonical state. This is the ADR-0018 property that lets us drop
//!    packets safely.
//! 4. **Full fallback.** A `Dirty::Full` (here: alt-screen toggle) must
//!    cause `synthesize_incremental` to produce the same bytes as
//!    `synthesize` (a full reset + paint), modulo bytes that depend on
//!    the per-instance `RenderState`'s prior dirty bookkeeping. We assert
//!    the load-bearing properties: reset preamble present, dimensions
//!    match, and the bytes round-trip to a fresh mirror grid that matches
//!    the canonical grid.
//!
//! libghostty types are `!Send + !Sync`; everything runs on the test
//! thread. No tokio runtime needed — this is a pure synthesizer test.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use libghostty_vt::render::{CellIterator, RenderState, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions};
use phux_server::grid::SnapshotSynthesizer;

/// Allocate a fresh `Terminal` with a small scrollback budget — matches
/// the existing `tests/common/screen.rs` shape so behaviour is
/// representative.
fn fresh(cols: u16, rows: u16) -> GhosttyTerminal<'static, 'static> {
    GhosttyTerminal::new(TerminalOptions {
        cols,
        rows,
        max_scrollback: 100,
    })
    .expect("Terminal::new")
}

/// Walk a `Terminal`'s viewport into a `Vec<String>`, skipping
/// `CellWide::SpacerTail` so wide glyphs aren't double-counted. Mirrors
/// the helper in `crates/phux-server/src/grid.rs`'s tests.
fn render_grid(t: &GhosttyTerminal<'_, '_>) -> Vec<String> {
    let mut rs = RenderState::new().expect("RenderState::new");
    let snap = rs.update(t).expect("update");
    let rows_n = snap.rows().expect("rows");
    let mut row_iter_storage = RowIterator::new().expect("RowIterator::new");
    let mut cell_iter_storage = CellIterator::new().expect("CellIterator::new");
    let mut row_iter = row_iter_storage.update(&snap).expect("row update");
    let mut grid: Vec<String> = Vec::with_capacity(usize::from(rows_n));
    let mut i: u16 = 0;
    while let Some(row) = row_iter.next() {
        if i >= rows_n {
            break;
        }
        let mut line = String::new();
        let mut cell_iter = cell_iter_storage.update(row).expect("cell update");
        while let Some(cell) = cell_iter.next() {
            let wide = cell.raw_cell().expect("raw_cell").wide().expect("wide");
            if matches!(wide, CellWide::SpacerTail) {
                continue;
            }
            let graphemes = cell.graphemes().expect("graphemes");
            if graphemes.is_empty() {
                line.push(' ');
            } else {
                for ch in &graphemes {
                    line.push(*ch);
                }
            }
        }
        grid.push(line);
        i += 1;
    }
    grid
}

#[test]
fn incremental_clean_returns_empty_bytes() {
    // Contract: when the consumer's `RenderState` is fully in sync with
    // the canonical and nothing has changed, `synthesize_incremental`
    // emits empty `replay_bytes`. Dimensions are always reported.
    let mut canonical = fresh(20, 5);
    canonical.vt_write(b"hello world");

    let mut synth = SnapshotSynthesizer::new().expect("SnapshotSynthesizer::new");
    let baseline = synth.synthesize(&canonical).expect("baseline synthesize");
    assert!(!baseline.bytes.is_empty(), "baseline must paint something");

    // Simulate FRAME_ACK arriving for the baseline seq.
    synth.mark_synced(&canonical).expect("mark_synced");

    // Second call against the same canonical with no intervening writes.
    let incremental = synth
        .synthesize_incremental(&canonical)
        .expect("synthesize_incremental");

    assert_eq!(
        incremental.cols, baseline.cols,
        "incremental must report the same cols",
    );
    assert_eq!(
        incremental.rows, baseline.rows,
        "incremental must report the same rows",
    );
    assert!(
        incremental.bytes.is_empty(),
        "post-ack Clean must emit empty bytes; got {} bytes starting with {:?}",
        incremental.bytes.len(),
        &incremental.bytes[..incremental.bytes.len().min(16)],
    );
}

#[test]
fn incremental_partial_bytes_advance_mirror_to_canonical() {
    // Canonical: bring it to a baseline state, capture the baseline via
    // `synthesize`, apply to mirror. Mirror is now at baseline.
    let mut canonical = fresh(20, 5);
    canonical.vt_write(b"line one");

    let mut synth = SnapshotSynthesizer::new().expect("SnapshotSynthesizer::new");
    let baseline = synth.synthesize(&canonical).expect("baseline");

    let mut mirror = fresh(baseline.cols, baseline.rows);
    mirror.vt_write(&baseline.bytes);

    let baseline_grid = render_grid(&mirror);
    assert_eq!(
        baseline_grid[0].trim_end(),
        "line one",
        "mirror grid should match canonical at baseline",
    );

    // Simulate FRAME_ACK for the baseline seq: the mirror is caught up,
    // so the synthesizer's reference state advances. After this, only
    // *new* changes on the canonical should produce diff bytes.
    synth.mark_synced(&canonical).expect("ack baseline");

    // Now mutate the canonical with new content — a fresh row on row 1.
    canonical.vt_write(b"\r\nline two");

    // Incremental synthesis: must produce a non-empty diff and, when
    // applied to the already-at-baseline mirror, advance it to match the
    // canonical.
    let diff = synth
        .synthesize_incremental(&canonical)
        .expect("synthesize_incremental");
    assert!(
        !diff.bytes.is_empty(),
        "Dirty::Partial after vt_write must emit non-empty bytes",
    );

    mirror.vt_write(&diff.bytes);

    let canonical_grid = render_grid(&canonical);
    let mirror_grid = render_grid(&mirror);
    assert_eq!(
        canonical_grid, mirror_grid,
        "mirror grid must match canonical after applying incremental diff;\n\
         canonical = {canonical_grid:?}\n\
         mirror    = {mirror_grid:?}",
    );
}

#[test]
fn incremental_loss_tolerance_two_emissions_both_reach_canonical() {
    // ADR-0018 loss-tolerance invariant: the synthesizer does NOT clear
    // dirty bits on emission. The tick driver clears them only on
    // FRAME_ACK. So two back-to-back `synthesize_incremental` calls
    // (modelling "first packet was lost, retry") must both produce a
    // diff that brings a baseline mirror to the canonical state. They
    // need not be byte-identical — only state-equivalent.
    let mut canonical = fresh(20, 5);
    canonical.vt_write(b"first");

    let mut synth = SnapshotSynthesizer::new().expect("SnapshotSynthesizer::new");
    let baseline = synth.synthesize(&canonical).expect("baseline");

    // Build two independent mirrors at baseline.
    let mut mirror_a = fresh(baseline.cols, baseline.rows);
    mirror_a.vt_write(&baseline.bytes);
    let mut mirror_b = fresh(baseline.cols, baseline.rows);
    mirror_b.vt_write(&baseline.bytes);

    // Sanity: both mirrors are at baseline before the canonical mutates.
    assert_eq!(render_grid(&mirror_a), render_grid(&mirror_b));

    // Simulate FRAME_ACK for baseline: synthesizer now considers the
    // mirrors caught up. The loss-tolerance invariant is exercised
    // *after* this point — any subsequent incremental call must remain
    // re-emittable until the next ack.
    synth.mark_synced(&canonical).expect("ack baseline");

    // Canonical advances.
    canonical.vt_write(b"\r\nsecond");

    // Emission 1 (mirror_a). Do NOT clear dirty bits afterwards —
    // `synthesize_incremental` is contractually obliged not to touch
    // them.
    let diff1 = synth.synthesize_incremental(&canonical).expect("diff1");
    assert!(
        !diff1.bytes.is_empty(),
        "first incremental after vt_write must be non-empty",
    );

    // Emission 2 (mirror_b). Because dirty bits were not cleared, the
    // second call must still see the same change set. (If the
    // synthesizer had cleared dirty under the hood, this call would
    // emit empty bytes and mirror_b would fail to catch up.)
    let diff2 = synth.synthesize_incremental(&canonical).expect("diff2");
    assert!(
        !diff2.bytes.is_empty(),
        "loss-tolerance invariant: second incremental emission without \
         FRAME_ACK must still be non-empty; an unacked diff must remain \
         re-emittable (ADR-0018)",
    );

    mirror_a.vt_write(&diff1.bytes);
    mirror_b.vt_write(&diff2.bytes);

    let canonical_grid = render_grid(&canonical);
    let grid_a = render_grid(&mirror_a);
    let grid_b = render_grid(&mirror_b);

    assert_eq!(
        canonical_grid, grid_a,
        "mirror_a (diff1) must reach canonical state",
    );
    assert_eq!(
        canonical_grid, grid_b,
        "mirror_b (diff2, after a 'lost' diff1) must also reach canonical \
         state — that is the loss-tolerance invariant",
    );
}

#[test]
fn incremental_full_dirty_falls_back_to_full_reset() {
    // Trigger `Dirty::Full` and assert the incremental path produces a
    // full-reset-class blob: starts with the DECSTR + ED 2 + CUP home
    // preamble, has the right dimensions, and round-trips to the
    // canonical grid on a fresh mirror.
    //
    // Alt-screen toggle is the canonical "global state changed" event
    // libghostty surfaces as `Dirty::Full` (per render.h: "global state
    // changed; renderer should redraw everything"). DECSET 1049 enters
    // the alt screen.
    let mut canonical = fresh(20, 5);
    canonical.vt_write(b"primary");

    // Use a dedicated synthesizer per call so the `RenderState`'s
    // accumulated dirty bits don't carry over between the two
    // syntheses we want to compare. We assert *bytes-equality* of the
    // full-path output and the incremental-Full-fallback output by
    // running each one on a clean synthesizer state.
    let synth_a = SnapshotSynthesizer::new().expect("synth_a");
    let mut synth_b = SnapshotSynthesizer::new().expect("synth_b");

    // Push the canonical into alt screen, write content there. Both the
    // mode change and the alt-screen paint contribute to dirtying.
    canonical.vt_write(b"\x1b[?1049h");
    canonical.vt_write(b"alt content");

    let from_full = synth_a.synthesize(&canonical).expect("synth_a full");
    let from_incremental = synth_b
        .synthesize_incremental(&canonical)
        .expect("synth_b incremental");

    // Dimensions agree.
    assert_eq!(from_full.cols, from_incremental.cols);
    assert_eq!(from_full.rows, from_incremental.rows);

    // The incremental-Full branch is documented to take the full reset
    // path; both blobs must therefore start with the reset preamble.
    assert!(
        from_incremental.bytes.starts_with(b"\x1b[!p\x1b[2J\x1b[H"),
        "Dirty::Full fallback must start with DECSTR + ED 2 + CUP home; \
         got first 16 bytes: {:?}",
        &from_incremental.bytes[..from_incremental.bytes.len().min(16)],
    );
    assert!(
        from_full.bytes.starts_with(b"\x1b[!p\x1b[2J\x1b[H"),
        "synthesize() must start with the same preamble",
    );

    // The two blobs should be byte-identical: both ran on a fresh
    // synthesizer against the same canonical, both took the full-reset
    // path, both shared the same per-cell + epilogue helpers.
    assert_eq!(
        from_full.bytes,
        from_incremental.bytes,
        "synthesize_incremental on Dirty::Full must produce identical \
         bytes to synthesize() (shared codepath via the full-reset \
         helper); diff lengths were {} vs {}",
        from_full.bytes.len(),
        from_incremental.bytes.len(),
    );

    // Round-trip: both blobs reproduce the canonical grid on fresh mirrors.
    let mut mirror_full = fresh(from_full.cols, from_full.rows);
    mirror_full.vt_write(&from_full.bytes);
    let mut mirror_incremental = fresh(from_incremental.cols, from_incremental.rows);
    mirror_incremental.vt_write(&from_incremental.bytes);

    let canonical_grid = render_grid(&canonical);
    let full_grid = render_grid(&mirror_full);
    let incremental_grid = render_grid(&mirror_incremental);

    assert_eq!(
        canonical_grid, full_grid,
        "full-synthesize round-trip must reach canonical",
    );
    assert_eq!(
        canonical_grid, incremental_grid,
        "Dirty::Full incremental round-trip must reach canonical",
    );
}
