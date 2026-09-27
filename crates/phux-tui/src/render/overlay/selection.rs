//! The shared copy-mode selection contract (ADR-0045).
//!
//! Plain data the selection UX (`copy_mode`) and the pane renderer (`attach::render`) both
//! import, so the highlight and the copy path can never disagree about which
//! cells a selection covers. Selection is a client-local projection, never a
//! wire tier (ADR-0030).

/// How copy-mode interprets the selection rectangle.
///
/// `Char` linear (the default), `Line` whole lines, `Rect` block. Client-local UI state.
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

/// A copy-mode selection in pane-local viewport cells.
///
/// Inclusive, zero-based, `start <= end`: what the renderer inverts and the copy path resolves.
/// `rectangle` false is linear (partial first/last rows), true is a column
/// band on every row.
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
    /// A `SelectionRect` from a normalized rectangle; `rectangle` iff `mode` is
    /// [`SelectionMode::Rect`].
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

    /// Whether the pane-local cell `(row, col)` is selected: linear clips the
    /// first and last rows; columnar clips every row to the column band.
    #[must_use]
    pub const fn contains(self, row: u16, col: u16) -> bool {
        if row < self.start_row || row > self.end_row {
            return false;
        }
        if self.rectangle {
            // Upstream orders corners by (row, col), so a down-and-left block
            // arrives with `start_col > end_col`; normalize per axis like
            // libghostty's `Selection::new` does.
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

/// How the dispatcher derives the selection from a [`CopyRequest`].
///
/// `Rect` uses the two corners; the others are engine-derived at the overlay cursor
/// (libghostty `select_*`), resolved in `attach/copy.rs` so this layer never
/// imports the engine.
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

/// A cell in the full primary-screen coordinate space: stays on the same
/// terminal cell while the user scrolls history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenSelectionPoint {
    /// Column, zero-based.
    pub col: u16,
    /// Row in the full screen (history plus active area), zero-based.
    pub row: u32,
}

/// A client-local copy request (ADR-0045): the normalized viewport rectangle,
/// block-vs-linear, and how `grab` derives the selection (corners for
/// [`SelectionGrab::Rect`], else the overlay cursor).
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
    /// Stable press point of a mouse drag; `None` for keyboard copy-mode.
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
        // A down-and-left block drag still selects the [2, 5] band per row.
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
