//! Pane dividers and borders — ratatui composer.
//!
//! Replaces `attach::multi_pane::paint_dividers` with a ratatui-based
//! composer that respects the **skip-cell carve-out** invariant from
//! ADR-0020: every cell inside a pane interior `Rect` is marked
//! `Cell::skip` so libghostty's direct VT output owns those cells
//! exclusively. The composer only emits VT bytes for the divider cells
//! between panes — never for pane interiors.
//!
//! # Architecture
//!
//! `compute_layout` (in `attach::multi_pane`) stays pure data: it walks
//! the layout tree and yields per-pane `Rect`s plus a list of
//! pre-resolved `DividerCell`s (one per box-drawing cell, with the
//! correct heavy/light/junction glyph already baked in). This module
//! consumes that data:
//!
//! 1. Allocate a ratatui `Buffer` covering the full viewport.
//! 2. Mark every cell inside a pane interior `Rect` with
//!    `set_diff_option(CellDiffOption::Skip)`.
//! 3. Write each `DividerCell` glyph into the buffer at its `(x, y)`.
//! 4. Emit positioned VT bytes for non-skip cells only.
//!
//! Step 4 is hand-rolled (not via `CrosstermBackend`) because:
//! - There is no previous-frame buffer to diff against; we always paint
//!   from scratch (the orchestrator owns frame-level invalidation).
//! - We must not touch any pane interior cell — even a no-op write would
//!   stomp libghostty's SGR / cursor state.
//!
//! # Invariants
//!
//! - **Skip-cell**: VT bytes are emitted only for non-skip cells. A unit
//!   test (`skip_cells_never_emit_vt`) asserts no CUP target lands
//!   inside any pane interior `Rect`. THIS IS LOAD-BEARING — if it
//!   breaks, libghostty's pane output gets stomped.
//! - **SGR reset**: emits `\x1b[0m` before any divider paint and again
//!   after the last cell, so leftover SGR from a prior pane render or
//!   from a divider's own styling never bleeds into the next paint.
//! - **No cursor positioning at exit**: the focused-pane render runs
//!   after dividers and is responsible for the final cursor placement.

use std::io::{self, Write};

use phux_protocol::TerminalId;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect as RataRect;

use crate::attach::multi_pane::PaneLayout;
use crate::render::overlay::help::HardcodedBinding;

/// The divider drag-resize table for the help overlay's `Mouse & menus`
/// section (phux-i0e8.10.3). COLOCATED with the divider layer: the drag
/// itself is driven by the dispatcher's ADR-0048 grab, whose targets are
/// the `divider_hits` that `compute_layout` produces alongside the glyph
/// cells this module paints. The `help_table_targets_exist` adjacency
/// test asserts a split layout actually yields those grab targets.
pub static HELP_BINDINGS: &[HardcodedBinding] = &[HardcodedBinding {
    chord: "drag divider",
    action: "resize the panes either side",
}];

/// Render the divider layer for `layout` to `out` using a ratatui buffer
/// with pane-interior cells marked `skip`.
///
/// `_focused` is accepted for symmetry with future per-focus styling
/// hooks; today the per-cell heavy/light resolution already lives in
/// `compute_layout`'s rasterizer (which consumes `PaneLayout::focus`),
/// so this composer just paints whatever glyphs that pass produced.
///
/// # Behavior
///
/// - No-op when `layout.dividers` is empty (single-pane attach).
/// - Emits a leading `\x1b[0m` SGR reset, then one positioned single-cell
///   paint per divider, then a trailing `\x1b[0m`.
/// - **Does not** emit a final cursor position. The focused pane's
///   render runs after this and owns cursor placement, per the
///   chrome ↔ pane handoff documented in ADR-0020.
/// - Pane interior cells are marked `Cell::skip` and never emitted —
///   libghostty owns those cells exclusively.
///
/// # Errors
///
/// Forwards any `io::Error` from `out`.
pub fn render_dividers<W: Write>(
    out: &mut W,
    layout: &PaneLayout,
    _focused: Option<&TerminalId>,
) -> io::Result<()> {
    if layout.dividers.is_empty() {
        return Ok(());
    }
    let (cols, rows) = layout.viewport;
    if cols == 0 || rows == 0 {
        return Ok(());
    }

    let buffer = compose_buffer(layout);
    emit_buffer(out, &buffer)
}

/// Build the ratatui `Buffer` for the divider layer.
///
/// Public-in-crate so the skip-cell invariant test can introspect the
/// buffer without re-emitting bytes, and so the rendered-frame compositor
/// (`phux snapshot --rendered`, phux-l5xa) can overlay the divider cells
/// into its dense cell grid without re-parsing emitted VT.
pub(crate) fn compose_buffer(layout: &PaneLayout) -> Buffer {
    let (cols, rows) = layout.viewport;
    let area = RataRect::new(0, 0, cols, rows);
    let mut buf = Buffer::empty(area);

    // 1. Mark every cell inside a pane interior Rect as skip. The
    //    libghostty pane renderer owns those cells; ratatui must not
    //    emit bytes into them. We touch cells via `cell_mut` (bounds-
    //    checked) so a degenerate Rect that escapes the viewport
    //    silently no-ops instead of panicking.
    for rect in layout.rects.values() {
        let x0 = rect.x;
        let y0 = rect.y;
        let x1 = rect.x.saturating_add(rect.w).min(cols);
        let y1 = rect.y.saturating_add(rect.h).min(rows);
        for y in y0..y1 {
            for x in x0..x1 {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_diff_option(CellDiffOption::Skip);
                }
            }
        }
    }

    // 2. Paint each divider glyph. compute_layout has already resolved
    //    heavy/light weights and junction shapes per cell, so we just
    //    drop the symbol in. Setting the symbol does NOT clear skip on
    //    its own, so we explicitly unset skip here — without that,
    //    a degenerate layout where a divider cell overlapped a pane
    //    rect would silently drop the divider.
    let mut sbuf = [0u8; 4];
    for cell in &layout.dividers {
        if cell.x >= layout.viewport.0 || cell.y >= layout.viewport.1 {
            continue;
        }
        let symbol = cell.ch.encode_utf8(&mut sbuf);
        if let Some(c) = buf.cell_mut((cell.x, cell.y)) {
            c.set_symbol(symbol);
            c.set_diff_option(CellDiffOption::None);
        }
    }

    buf
}

/// Emit non-skip cells in `buf` as positioned single-cell VT paints.
///
/// Iteration is row-major (matches `paint_dividers`' ordering so any
/// downstream byte-equality regressions surface). Each emitted cell
/// gets its own `CUP` so we don't depend on terminal auto-wrap behavior
/// at the right margin.
///
/// Skip is the only filter — cells with an empty symbol but skip=false
/// (shouldn't happen for our composer, but defensive) are also skipped
/// so we don't paint stray spaces over future overlay layers.
fn emit_buffer<W: Write>(out: &mut W, buf: &Buffer) -> io::Result<()> {
    out.write_all(b"\x1b[0m")?;
    let area = buf.area;
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            // `(x, y)` is in-bounds by construction (we iterate `area`),
            // but `cell` is `Option`-returning; treat any miss as a
            // silent skip rather than panic — keeps the chrome layer
            // resilient if a future ratatui upgrade changes bounds
            // semantics.
            let Some(cell) = buf.cell((x, y)) else {
                continue;
            };
            if cell.diff_option == CellDiffOption::Skip {
                continue;
            }
            let sym = cell.symbol();
            if sym.is_empty() || sym == " " {
                continue;
            }
            // CUP is 1-based.
            let r = y.saturating_add(1);
            let c = x.saturating_add(1);
            write!(out, "\x1b[{r};{c}H{sym}")?;
        }
    }
    // Trailing reset so the next layer (status bar, focused pane render)
    // doesn't inherit any divider-cell SGR. Today divider cells are
    // plain (no fg/bg), but defending against future styling here costs
    // 4 bytes and saves a class of cross-layer regressions.
    out.write_all(b"\x1b[0m")?;
    out.flush()
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::attach::multi_pane::{DividerCell, compute_layout};
    use crate::layout::{LayoutNode, LayoutState, Rect, SplitDir, split_at};

    fn t(id: u32) -> TerminalId {
        TerminalId::local(id)
    }

    fn leaf(id: u32) -> LayoutNode {
        LayoutNode::Leaf(t(id))
    }

    /// Empty layout: no dividers, no bytes emitted.
    #[test]
    fn empty_layout_is_noop() {
        let layout = PaneLayout {
            viewport: (80, 24),
            rects: HashMap::new(),
            dividers: Vec::new(),
            divider_hits: Vec::new(),
        };
        let mut buf: Vec<u8> = Vec::new();
        render_dividers(&mut buf, &layout, None).unwrap();
        assert!(buf.is_empty());
    }

    /// Zero-axis viewport: no-op.
    #[test]
    fn zero_viewport_is_noop() {
        let layout = PaneLayout {
            viewport: (0, 24),
            rects: HashMap::new(),
            dividers: vec![DividerCell {
                x: 0,
                y: 0,
                ch: '\u{2502}',
            }],
            divider_hits: Vec::new(),
        };
        let mut buf: Vec<u8> = Vec::new();
        render_dividers(&mut buf, &layout, None).unwrap();
        assert!(buf.is_empty());
    }

    /// Two-pane horizontal split: every emitted byte targets the
    /// divider column; no CUP lands inside either pane's interior.
    /// THIS IS THE LOAD-BEARING SKIP-CELL INVARIANT TEST.
    #[test]
    fn skip_cells_never_emit_vt() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        let layout = compute_layout(&state, (80, 24));
        assert!(
            !layout.dividers.is_empty(),
            "test precondition: have dividers"
        );

        let mut bytes: Vec<u8> = Vec::new();
        render_dividers(&mut bytes, &layout, Some(&t(1))).unwrap();
        let s = String::from_utf8(bytes).unwrap();

        let pane_rects: Vec<Rect> = layout.rects.values().copied().collect();
        let cups = extract_cups(&s);
        assert!(!cups.is_empty(), "expected at least one CUP");
        for (row_1b, col_1b) in cups {
            // Convert to 0-based outer-viewport coords.
            let y = row_1b.saturating_sub(1);
            let x = col_1b.saturating_sub(1);
            for r in &pane_rects {
                assert!(
                    !rect_contains(*r, x, y),
                    "CUP at ({x}, {y}) landed inside pane interior rect {r:?} — \
                     skip-cell invariant violated"
                );
            }
        }
    }

    /// Three-pane cross split: same skip invariant holds across T-piece
    /// junctions and inner dividers.
    #[test]
    fn skip_cells_invariant_holds_for_cross_split() {
        let t1 = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let t2 = split_at(&t1, &t(1), &t(3), SplitDir::Vertical, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(t2),
            focus: Some(t(2)),
        };
        let layout = compute_layout(&state, (80, 24));
        let mut bytes: Vec<u8> = Vec::new();
        render_dividers(&mut bytes, &layout, Some(&t(2))).unwrap();
        let s = String::from_utf8(bytes).unwrap();

        let pane_rects: Vec<Rect> = layout.rects.values().copied().collect();
        for (row_1b, col_1b) in extract_cups(&s) {
            let y = row_1b.saturating_sub(1);
            let x = col_1b.saturating_sub(1);
            for r in &pane_rects {
                assert!(
                    !rect_contains(*r, x, y),
                    "CUP at ({x}, {y}) landed inside pane interior rect {r:?}"
                );
            }
        }
    }

    /// Emitted bytes start with an SGR reset and end with one.
    #[test]
    fn emits_leading_and_trailing_sgr_reset() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        let layout = compute_layout(&state, (80, 24));
        let mut bytes: Vec<u8> = Vec::new();
        render_dividers(&mut bytes, &layout, Some(&t(1))).unwrap();
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("\x1b[0m"), "expected leading SGR reset");
        assert!(s.ends_with("\x1b[0m"), "expected trailing SGR reset");
    }

    /// Visual parity: heavy vertical bar appears for a focused-adjacent
    /// vertical divider, matching the prior `paint_dividers` output.
    #[test]
    fn heavy_glyph_present_when_focus_adjacent() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        let layout = compute_layout(&state, (80, 24));
        let mut bytes: Vec<u8> = Vec::new();
        render_dividers(&mut bytes, &layout, Some(&t(1))).unwrap();
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains('\u{2503}'), "expected heavy │ in divider output");
    }

    /// Buffer-level introspection: every cell inside a pane Rect has
    /// `skip = true` after compose; every divider cell has `skip =
    /// false` and the right symbol. This is the structural twin of
    /// `skip_cells_never_emit_vt` — if compose breaks, this catches it
    /// before emit even runs.
    #[test]
    fn compose_buffer_marks_pane_interiors_skip() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        let layout = compute_layout(&state, (80, 24));
        let buf = compose_buffer(&layout);
        for r in layout.rects.values() {
            for y in r.y..r.y + r.h {
                for x in r.x..r.x + r.w {
                    let cell = buf.cell((x, y)).expect("in-bounds");
                    assert!(
                        cell.diff_option == CellDiffOption::Skip,
                        "pane interior cell ({x}, {y}) in {r:?} not marked skip"
                    );
                }
            }
        }
        for d in &layout.dividers {
            let cell = buf.cell((d.x, d.y)).expect("in-bounds");
            assert!(
                cell.diff_option != CellDiffOption::Skip,
                "divider cell ({}, {}) marked skip",
                d.x,
                d.y
            );
            assert_eq!(
                cell.symbol().chars().next(),
                Some(d.ch),
                "divider cell at ({}, {}) symbol mismatch",
                d.x,
                d.y
            );
        }
    }

    /// phux-i0e8.10.3: the help table advertises drag-to-resize, so the
    /// drag targets must actually exist — a split layout yields the
    /// `divider_hits` the dispatcher's ADR-0048 grab resolves against
    /// (the drag behavior itself is held by `input_dispatch`'s ADR-0048
    /// tests). If dividers stop producing hit targets, the advertised
    /// gesture would be dead and this fails.
    #[test]
    fn help_table_targets_exist() {
        assert!(
            HELP_BINDINGS.iter().any(|b| b.chord.contains("drag")),
            "the table must advertise the drag gesture"
        );
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        let layout = compute_layout(&state, (80, 24));
        assert!(
            !layout.divider_hits.is_empty(),
            "a split layout must yield divider grab targets"
        );
    }

    // -- helpers ---------------------------------------------------------------

    /// Half-open rect-contains, matching `multi_pane::rect_contains`.
    fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
        x >= r.x && y >= r.y && x < r.x.saturating_add(r.w) && y < r.y.saturating_add(r.h)
    }

    /// Extract every CUP target `(row_1b, col_1b)` from a VT byte stream.
    /// Matches `\x1b[<row>;<col>H`. Tolerates the other SGR sequences
    /// (`\x1b[0m`) by skipping any `\x1b[` whose body isn't pure
    /// digit;digit;…H form.
    fn extract_cups(s: &str) -> Vec<(u16, u16)> {
        let mut out = Vec::new();
        let bytes = s.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
                // Find the terminator letter.
                let start = i + 2;
                let mut j = start;
                while j < bytes.len() && !bytes[j].is_ascii_alphabetic() {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'H' {
                    let body = std::str::from_utf8(&bytes[start..j]).unwrap_or("");
                    if let Some((r, c)) = body.split_once(';')
                        && let (Ok(rn), Ok(cn)) = (r.parse::<u16>(), c.parse::<u16>())
                    {
                        out.push((rn, cn));
                    }
                }
                i = j.saturating_add(1);
            } else {
                i += 1;
            }
        }
        out
    }
}
