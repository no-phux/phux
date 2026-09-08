//! The shared copy-mode selection contract (ADR-0045).
//!
//! Copy-mode is a *client-local projection* over the focused pane's own
//! libghostty engine — never a wire tier
//! ([ADR-0030](../../../../../ADR/0030-engine-delegated-wire-and-projection-consumers.md),
//! [ADR-0045](../../../../../ADR/0045-client-side-copy-mode.md)). Two consumers
//! of that projection — the selection UX (`copy_mode`) and the pane renderer
//! (`attach::render`) — must agree, byte for byte, on what a selection is and
//! which cells it covers. This module is the single leaf where that agreement
//! lives: it is **plain data only** and imports neither the overlay state
//! machine, the renderer, ratatui, nor libghostty. Both sides depend on this
//! module; this module depends on neither, so the block-highlight geometry and
//! the copy path can never disagree about what a block selection covers.
//!
//! Everything here was previously scattered across `copy_mode.rs`
//! (`SelectionMode`), `attach/render.rs` (`SelectionRect`), and `overlay/mod.rs`
//! (`SelectionGrab`, `CopyRequest`); ADR-0045 relocates it into one owner.

/// How copy-mode interprets the selection rectangle.
///
/// Client-local UI state ([ADR-0030], [ADR-0045]): selection is a consumer-side
/// projection, so the mode lives with the client rather than on the wire.
/// `Char` is the default linear selection; `Line` selects whole lines; `Rect`
/// is Mosh-style rectangular (block/columnar) selection.
///
/// [ADR-0030]: ../../../../../ADR/0030-engine-delegated-wire-and-projection-consumers.md
/// [ADR-0045]: ../../../../../ADR/0045-client-side-copy-mode.md
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionMode {
    /// Character-wise (linear) selection — the default.
    #[default]
    Char,
    /// Line-wise selection (whole lines).
    Line,
    /// Rectangular (block) selection.
    Rect,
}

/// A copy-mode selection in pane-local viewport cells (inclusive).
///
/// This is the exact geometry the renderer reverse-videos while painting and
/// the copy path resolves against the engine, so the two cannot drift. The
/// `rectangle` flag is the shared discriminant: `false` is a linear (text-flow)
/// selection — full interior rows, partial first/last rows; `true` is a
/// columnar (block) selection — the intersection of the row span and the
/// `[start_col, end_col]` column band on *every* row.
///
/// Coordinates are pane-local viewport cells, zero-based, normalized so that
/// `start <= end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionRect {
    /// First selected row (inclusive).
    pub start_row: u16,
    /// First selected column, on `start_row` (inclusive).
    pub start_col: u16,
    /// Last selected row (inclusive).
    pub end_row: u16,
    /// Last selected column, on `end_row` (inclusive).
    pub end_col: u16,
    /// Block (columnar) selection when `true`; linear (text-flow) when `false`.
    pub rectangle: bool,
}

impl SelectionRect {
    /// Build a `SelectionRect` from a normalized inclusive rectangle and a
    /// [`SelectionMode`]. `rectangle` is set iff `mode` is
    /// [`SelectionMode::Rect`]; `Char` and `Line` both produce linear geometry.
    #[must_use]
    pub fn from_range(
        start_row: u16,
        start_col: u16,
        end_row: u16,
        end_col: u16,
        mode: SelectionMode,
    ) -> Self {
        Self {
            start_row,
            start_col,
            end_row,
            end_col,
            rectangle: mode == SelectionMode::Rect,
        }
    }

    /// Whether the pane-local cell `(row, col)` falls inside the selection.
    ///
    /// Branches on [`Self::rectangle`]:
    /// - **linear** (`false`): full interior rows; the first row is clipped to
    ///   `>= start_col` and the last row to `<= end_col`.
    /// - **columnar** (`true`): every row in the span is clipped to the
    ///   `[min(start_col, end_col), max(start_col, end_col)]` column band, so an
    ///   interior-row cell outside that band is excluded even though a linear
    ///   selection would include it. The band is min/max-normalized because the
    ///   corners are ordered lexicographically by `(row, col)` upstream, which
    ///   can leave `start_col > end_col` for a down-and-left block drag.
    #[must_use]
    pub const fn contains(self, row: u16, col: u16) -> bool {
        if row < self.start_row || row > self.end_row {
            return false;
        }
        if self.rectangle {
            // Columnar: the same column band on every row in the span. The two
            // corners are normalized lexicographically by (row, col) upstream
            // (`copy_mode::CellRange::from_points`), so a block whose lower row
            // carries the smaller column arrives with `start_col > end_col`.
            // Clamp to the `[min, max]` band on the column axis — matching how
            // libghostty normalizes block corners per-axis in `Selection::new`,
            // so the highlight covers exactly the cells the copy path extracts.
            let (lo, hi) = if self.start_col <= self.end_col {
                (self.start_col, self.end_col)
            } else {
                (self.end_col, self.start_col)
            };
            return col >= lo && col <= hi;
        }
        // Linear: partial first/last rows, full interior rows.
        if row == self.start_row && col < self.start_col {
            return false;
        }
        if row == self.end_row && col > self.end_col {
            return false;
        }
        true
    }
}

/// How the dispatcher should derive the selection from a [`CopyRequest`]
/// ([ADR-0045]).
///
/// `Rect` is the two-corner path: the overlay's `start`/`end` rectangle is
/// turned into a linear-or-block `Selection` directly. The remaining variants
/// are *engine-derived*: the dispatcher hands the overlay cursor
/// (`cursor_row`/`cursor_col`) to libghostty's `select_*` helpers, which return
/// a snapshot selection the dispatcher then formats. Keeping the derivation as a
/// tag here (not a `libghostty_vt` call) preserves the render-layer boundary —
/// `render/overlay/` never imports the engine; the bridge in `attach/copy.rs`
/// does the resolution.
///
/// [ADR-0045]: ../../../../../ADR/0045-client-side-copy-mode.md
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionGrab {
    /// Two-corner rectangle: `start`/`end` corners, block when
    /// [`CopyRequest::rectangle`] is set, else linear. The default.
    #[default]
    Rect,
    /// Word under the cursor (`select_word`).
    Word,
    /// Whole line under the cursor (`select_line`).
    Line,
    /// Whole line under the cursor, bounded by semantic-prompt state changes
    /// (`select_line` with `with_semantic_prompt_boundary(true)`).
    LineSemantic,
    /// All selectable terminal content (`select_all`).
    All,
    /// The command-output span under the cursor (`select_output`). Degrades
    /// to an empty no-op when the pane has no OSC-133 semantic zones.
    Output,
}

/// A cell in the terminal's full primary-screen coordinate space.
///
/// Unlike viewport coordinates, this stays attached to the same terminal
/// cell while the user scrolls through history. It is intentionally a plain
/// value: the engine resolves it only at copy time, so the overlay remains
/// independent of libghostty handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenSelectionPoint {
    /// Column, zero-based.
    pub col: u16,
    /// Row in the full screen (history plus active area), zero-based.
    pub row: u32,
}

/// A client-local copy request ([ADR-0045]).
///
/// The overlay's normalized, inclusive viewport selection rectangle, handed to
/// the bridge to resolve against the focused pane's own libghostty engine.
/// Coordinates are pane-local viewport cells (`row`/`col`, zero-based,
/// `start <= end`). `rectangle` selects block (vs linear) extraction.
///
/// `grab` tags how the bridge derives the selection. For
/// [`SelectionGrab::Rect`] (the default) the `start`/`end` corners drive a
/// two-corner `Selection`; the engine-derived grabs instead resolve at the
/// overlay cursor (`cursor_row`/`cursor_col`).
///
/// [ADR-0045]: ../../../../../ADR/0045-client-side-copy-mode.md
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyRequest {
    /// Top row of the selection (inclusive).
    pub start_row: u16,
    /// Left column of the selection (inclusive).
    pub start_col: u16,
    /// Bottom row of the selection (inclusive).
    pub end_row: u16,
    /// Right column of the selection (inclusive).
    pub end_col: u16,
    /// Stable press point for a mouse drag, when it was resolved by the
    /// dispatcher. Keyboard copy-mode keeps this `None` and remains purely
    /// viewport-relative.
    pub mouse_anchor_screen: Option<ScreenSelectionPoint>,
    /// Block (rectangular) selection when `true`; linear when `false`. Only
    /// consulted for [`SelectionGrab::Rect`].
    pub rectangle: bool,
    /// The overlay cursor row (pane-local viewport cell). Engine-derived
    /// grabs (`Word`/`Line`/`LineSemantic`/`Output`) resolve here.
    pub cursor_row: u16,
    /// The overlay cursor column (pane-local viewport cell).
    pub cursor_col: u16,
    /// How the bridge derives the selection from this request.
    pub grab: SelectionGrab,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_linear_partial_first_and_last_rows() {
        // Linear/text selection: full interior rows, partial first/last rows.
        let sel = SelectionRect {
            start_row: 1,
            start_col: 1,
            end_row: 3,
            end_col: 5,
            rectangle: false,
        };
        assert!(sel.contains(1, 1)); // start corner
        assert!(sel.contains(2, 0)); // interior row, any col
        assert!(sel.contains(2, 9)); // interior row, far col — linear spans it
        assert!(sel.contains(3, 5)); // end corner
        assert!(!sel.contains(0, 1)); // above
        assert!(!sel.contains(1, 0)); // before start on start row
        assert!(!sel.contains(3, 6)); // after end on end row
        assert!(!sel.contains(4, 1)); // below
    }

    #[test]
    fn contains_block_clips_every_row_to_the_column_band() {
        // Columnar/block selection: the [start_col, end_col] band on EVERY row.
        let sel = SelectionRect {
            start_row: 1,
            start_col: 2,
            end_row: 3,
            end_col: 5,
            rectangle: true,
        };
        // Inside the band on each row of the span.
        assert!(sel.contains(1, 2)); // band left edge, first row
        assert!(sel.contains(2, 3)); // interior row, inside band
        assert!(sel.contains(3, 5)); // band right edge, last row
        // Outside the column band, even on an interior row — this is the cell a
        // linear selection includes but a block one excludes.
        assert!(!sel.contains(2, 1)); // left of the band on an interior row
        assert!(!sel.contains(2, 6)); // right of the band on an interior row
        assert!(!sel.contains(1, 1)); // left of the band on the first row
        assert!(!sel.contains(3, 6)); // right of the band on the last row
        // Outside the row span.
        assert!(!sel.contains(0, 3));
        assert!(!sel.contains(4, 3));
    }

    #[test]
    fn block_and_linear_disagree_on_the_wrap_cell() {
        // Same two corners, different modes: the interior-row cell outside the
        // column band is in the linear selection but not the block one.
        let linear = SelectionRect {
            start_row: 0,
            start_col: 1,
            end_row: 1,
            end_col: 2,
            rectangle: false,
        };
        let block = SelectionRect {
            rectangle: true,
            ..linear
        };
        // Row 0, col 3 sits after `start` but outside the band.
        assert!(linear.contains(0, 3), "linear spans to the row end");
        assert!(!block.contains(0, 3), "block clips to the column band");
    }

    #[test]
    fn contains_block_normalizes_inverted_column_corners() {
        // Down-and-left block drag: anchor (row 0, col 5) to cursor (row 3,
        // col 2). `CellRange::from_points` orders corners lexicographically by
        // (row, col), so (0,5) <= (3,2) leaves start_col=5 > end_col=2. The
        // band must still be [2, 5] on every row — matching the cells
        // libghostty extracts — not the empty `col >= 5 && col <= 2`.
        let sel = SelectionRect {
            start_row: 0,
            start_col: 5,
            end_row: 3,
            end_col: 2,
            rectangle: true,
        };
        // Every row in the span carries the normalized [2, 5] band.
        for row in 0..=3 {
            assert!(sel.contains(row, 2), "row {row}: band left edge");
            assert!(sel.contains(row, 5), "row {row}: band right edge");
            assert!(sel.contains(row, 3), "row {row}: inside band");
            assert!(!sel.contains(row, 1), "row {row}: left of band");
            assert!(!sel.contains(row, 6), "row {row}: right of band");
        }
        // Outside the row span stays excluded.
        assert!(!sel.contains(4, 3));
    }

    #[test]
    fn from_range_sets_rectangle_iff_mode_is_rect() {
        let r = SelectionRect::from_range(0, 0, 2, 4, SelectionMode::Rect);
        assert!(r.rectangle);
        assert!(!SelectionRect::from_range(0, 0, 2, 4, SelectionMode::Char).rectangle);
        assert!(!SelectionRect::from_range(0, 0, 2, 4, SelectionMode::Line).rectangle);
        // Corners are carried through untouched.
        assert_eq!(
            (r.start_row, r.start_col, r.end_row, r.end_col),
            (0, 0, 2, 4)
        );
    }
}
