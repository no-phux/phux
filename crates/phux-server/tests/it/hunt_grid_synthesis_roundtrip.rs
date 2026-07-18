//! Round-trip correctness probes for `synthesize_against_reference`: the diff
//! bytes, applied to a mirror that started from the same snapshot, must
//! reconstruct the source grid across churn, alt-screen toggles, and resizes.
#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use libghostty_vt::render::{CellIterator, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::{RenderState, Terminal as GhosttyTerminal, TerminalOptions};
use phux_server::grid::{ConsumerReference, SnapshotSynthesizer, synthesize};

fn fresh(cols: u16, rows: u16) -> GhosttyTerminal<'static, 'static> {
    GhosttyTerminal::new(TerminalOptions {
        cols,
        rows,
        max_scrollback: 200,
    })
    .unwrap()
}

/// Project the viewport grid to text rows (wide-tail aware).
fn render_grid(t: &GhosttyTerminal<'_, '_>) -> Vec<String> {
    let mut rs = RenderState::new().unwrap();
    let snap = rs.update(t).unwrap();
    let rows_n = snap.rows().unwrap();
    let mut row_storage = RowIterator::new().unwrap();
    let mut cell_storage = CellIterator::new().unwrap();
    let mut row_iter = row_storage.update(&snap).unwrap();
    let mut grid = Vec::with_capacity(usize::from(rows_n));
    let mut i = 0u16;
    while let Some(row) = row_iter.next() {
        if i >= rows_n {
            break;
        }
        let mut line = String::new();
        let mut cells = cell_storage.update(row).unwrap();
        while let Some(cell) = cells.next() {
            let wide = cell.raw_cell().unwrap().wide().unwrap();
            if matches!(wide, CellWide::SpacerTail) {
                continue;
            }
            let g = cell.graphemes().unwrap();
            if g.is_empty() {
                line.push(' ');
            } else {
                for ch in &g {
                    line.push(*ch);
                }
            }
        }
        grid.push(line.trim_end().to_owned());
        i += 1;
    }
    grid
}

// Round-trip: a mirror primed from the snapshot, fed each reference diff,
// must equal the source after every churn round — including an alt-screen
// entry/exit in the middle.
#[test]
fn probe_reference_diff_roundtrip_with_alt_screen_churn() {
    let mut src = fresh(40, 10);
    src.vt_write(b"initial primary content\r\nsecond line");

    // Mirror starts from the full snapshot.
    let snap = synthesize(&src).unwrap();
    let mut mirror = fresh(snap.cols, snap.rows);
    mirror.vt_write(&snap.bytes);

    let mut synth = SnapshotSynthesizer::new().unwrap();
    let mut reference = ConsumerReference::new();
    synth.prime_reference(&src, &mut reference).unwrap();

    let rounds: &[&[u8]] = &[
        b"\r\nmore primary output",
        b"\x1b[?1049h",                // enter alt screen
        b"\x1b[2J\x1b[Halt screen!!!", // paint alt
        b"\r\nalt line two",
        b"\x1b[?1049l", // leave alt screen -> back to primary
        b"\r\nback on primary",
    ];

    for (n, chunk) in rounds.iter().enumerate() {
        src.vt_write(chunk);
        let diff = synth
            .synthesize_against_reference(&src, &mut reference)
            .unwrap();
        mirror.vt_write(&diff.bytes);
        let sg = render_grid(&src);
        let mg = render_grid(&mirror);
        assert_eq!(
            sg,
            mg,
            "mirror diverged from source after round {n} (chunk {:?})",
            String::from_utf8_lossy(chunk),
        );
    }
}

// Round-trip across a GROW resize mid-stream.
#[test]
fn probe_reference_diff_roundtrip_grow_resize() {
    let mut src = fresh(20, 5);
    src.vt_write(b"line one\r\nline two\r\nline three");

    let snap = synthesize(&src).unwrap();
    let mut mirror = fresh(snap.cols, snap.rows);
    mirror.vt_write(&snap.bytes);

    let mut synth = SnapshotSynthesizer::new().unwrap();
    let mut reference = ConsumerReference::new();
    synth.prime_reference(&src, &mut reference).unwrap();

    // Grow. The reference resets geometry; the diff should be a full repaint
    // that reconstructs the grid. The mirror must also be resized (the wire
    // carries dims; here we resize the mirror to match before applying).
    src.resize(40, 12, 8, 16).unwrap();
    src.vt_write(b"\r\nafter grow");
    let diff = synth
        .synthesize_against_reference(&src, &mut reference)
        .unwrap();
    mirror.resize(diff.cols, diff.rows, 8, 16).unwrap();
    mirror.vt_write(&diff.bytes);

    assert_eq!(
        render_grid(&src),
        render_grid(&mirror),
        "mirror must reconstruct source after a grow resize",
    );
}

// Many tiny independent edits across distant rows — verify only changed rows
// emit (partial-diff economy) yet the mirror still reconstructs.
#[test]
fn probe_reference_diff_sparse_edits_roundtrip() {
    let mut src = fresh(40, 30);
    for r in 0..30u16 {
        src.vt_write(format!("\x1b[{};1Hrow {r}", r + 1).as_bytes());
    }
    let snap = synthesize(&src).unwrap();
    let mut mirror = fresh(snap.cols, snap.rows);
    mirror.vt_write(&snap.bytes);

    let mut synth = SnapshotSynthesizer::new().unwrap();
    let mut reference = ConsumerReference::new();
    synth.prime_reference(&src, &mut reference).unwrap();

    // Edit only rows 5, 15, 25.
    for r in [5u16, 15, 25] {
        src.vt_write(format!("\x1b[{};1H\x1b[2KEDIT-{r}", r + 1).as_bytes());
    }
    let diff = synth
        .synthesize_against_reference(&src, &mut reference)
        .unwrap();
    mirror.vt_write(&diff.bytes);
    assert_eq!(
        render_grid(&src),
        render_grid(&mirror),
        "sparse-edit reconstruct"
    );
}
