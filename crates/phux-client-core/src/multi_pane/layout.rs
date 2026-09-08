use std::collections::HashMap;

use phux_protocol::TerminalId;

use crate::layout::{LayoutNode, LayoutState, Rect};

use super::rasterize::{
    DividerCell, DividerHit, DividerSegment, divider_hits, freeze_split_dim, min_dims, rasterize,
    walk_layout, walk_layout_proportional,
};

/// Result of [`compute_layout`]: per-pane rectangles plus the cells
/// occupied by dividers between them.
///
/// The pane `Rect`s are exactly those [`pane_rects`] returns; the divider
/// cells fill the **gaps** the layout algorithm carved out for them.
/// Together they cover the outer viewport with no overlap and no holes —
/// the `proptest_rects_and_dividers_tile_exactly` invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneLayout {
    /// The outer viewport this layout was computed against.
    pub viewport: (u16, u16),
    /// Per-pane bounding rectangle, in outer-viewport cell coordinates.
    pub rects: HashMap<TerminalId, Rect>,
    /// Divider cells with their pre-resolved box-drawing character.
    ///
    /// Each entry positions exactly one cell. The character carries the
    /// final weight (light or heavy per edge) and junction shape; the
    /// painter writes it verbatim. Order is row-major within each
    /// interior split (left-to-right then top-to-bottom).
    pub dividers: Vec<DividerCell>,
    /// Per-split grab targets for drag-to-resize.
    ///
    /// One [`DividerHit`] per interior split, mapping the cells that
    /// split's divider line occupies to the split's node path + axis. A
    /// mouse press that lands in any hit's `cells` resolves to the split
    /// whose `ratio` the drag adjusts (ADR-0048). Built from the same
    /// segments as [`Self::dividers`] and clamped to the same viewport,
    /// so a hit cell is always a painted divider cell and vice versa.
    pub divider_hits: Vec<DividerHit>,
}

// -----------------------------------------------------------------------------
// compute_layout — the public entry point
// -----------------------------------------------------------------------------

/// Compute per-pane rectangles and divider cells for `layout` inside a
/// `viewport_dims` outer viewport.
///
/// Returns an empty `PaneLayout` when `layout.tree.is_none()` (fresh
/// state, no panes seeded yet) or when either viewport axis is zero —
/// callers should special-case "single-pane attach" by checking
/// `dividers.is_empty()` and `rects.len() == 1`.
#[must_use]
pub fn compute_layout(layout: &LayoutState, viewport_dims: (u16, u16)) -> PaneLayout {
    let (cols, rows) = viewport_dims;
    compute_layout_in(
        layout,
        Rect {
            x: 0,
            y: 0,
            w: cols,
            h: rows,
        },
        viewport_dims,
    )
}

/// Tile the panes into `content`, an arbitrary sub-rectangle of the viewport.
///
/// Like [`compute_layout`] but inset: chrome that reserves edge space (a left
/// sidebar, say) passes the
/// residual content `Rect` so the pane rects, divider cells, and — via the
/// matching [`pane_rects_in`] used by reflow — the PTY sizing all agree on
/// the same offset. `viewport_dims` is still the **full** viewport: divider
/// rasterization clamps against it, so an inset pane's divider never escapes
/// the screen. With `content` at `(0, 0)` spanning the whole viewport this
/// is exactly [`compute_layout`].
#[must_use]
pub fn compute_layout_in(
    layout: &LayoutState,
    content: Rect,
    viewport_dims: (u16, u16),
) -> PaneLayout {
    let Some(tree) = layout.tree.as_ref() else {
        return PaneLayout {
            viewport: viewport_dims,
            rects: HashMap::new(),
            dividers: Vec::new(),
            divider_hits: Vec::new(),
        };
    };
    if content.w == 0 || content.h == 0 {
        return PaneLayout {
            viewport: viewport_dims,
            rects: HashMap::new(),
            dividers: Vec::new(),
            divider_hits: Vec::new(),
        };
    }

    // Walk the tree once, allocating rects to leaves within `content` and
    // emitting one `DividerSegment` per interior split. Both share the
    // same divider-budget math: each Horizontal split eats one column
    // from its bounds, each Vertical split eats one row.
    let mut segments: Vec<DividerSegment> = Vec::new();
    let mut rects: HashMap<TerminalId, Rect> = HashMap::new();
    walk_layout(tree, content, &mut segments, &mut rects);

    // Rasterize the segments into per-cell divider entries, resolving
    // junction SHAPES. Clamp to the full viewport, not `content` —
    // segments already carry inset coordinates, and the clamp only
    // guards the screen edge. Emphasis (which rules bound the focused
    // pane) is the chrome layer's job: it holds the client's
    // authoritative focus, which the layout tree's own `focus` can lag.
    let dividers = rasterize(&segments, viewport_dims);
    // Same segments, same viewport clamp: the grab map's cells are
    // exactly the cells `rasterize` paints a glyph into.
    let divider_hits = divider_hits(&segments, viewport_dims);

    PaneLayout {
        viewport: viewport_dims,
        rects,
        dividers,
        divider_hits,
    }
}

/// Per-leaf rectangle map for `tree` inside a `viewport_dims` outer
/// viewport — the canonical client tiling.
///
/// This is the exact same local-divider walk [`compute_layout`] paints
/// with, minus the divider rasterization. Reflow-emit
/// (`phux_tui::attach::reflow`) and the min-cell gate
/// (`phux_tui::attach::actions`) both call it so the size a pane's
/// PTY is told to be (via `TERMINAL_RESIZE`) equals the rect it is
/// painted into, by construction — closing the gap/overlap class of bug
/// that arose when reflow and paint used divergent algorithms.
///
/// Every leaf of `tree` receives a rect; sub-viable splits yield
/// zero-size rects rather than dropping leaves (the same exact-tiling
/// walk [`compute_layout`] uses). `viewport_dims` is the **outer** viewport —
/// divider accounting happens *inside* the walk, so callers pass the
/// full pane viewport, never a pre-deducted content rectangle.
#[must_use]
pub fn pane_rects(tree: &LayoutNode, viewport_dims: (u16, u16)) -> HashMap<TerminalId, Rect> {
    pane_rects_in(
        tree,
        Rect {
            x: 0,
            y: 0,
            w: viewport_dims.0,
            h: viewport_dims.1,
        },
    )
}

/// The cell span available to the split at `path` for its `ratio`, in
/// outer-viewport coordinates along the split's axis (ADR-0048).
///
/// Returns `(start, content_len)` where `start` is the first cell of the
/// split's bounds on its axis and `content_len` is the budget the ratio
/// divides (the split's axis length minus the one cell reserved for its
/// own divider). A drag converts a pointer cell `p` to a ratio via
/// `(p - start) / content_len`, which positions the divider under the
/// pointer. `None` when `path` does not address a `Split` inside
/// `content` or the budget is zero (a sub-viable split — the caller
/// bells). Walks the same divider-reservation geometry as
/// [`compute_layout_in`], so the span matches the painted divider.
#[must_use]
pub fn split_content_span_at(
    tree: &LayoutNode,
    content: Rect,
    path: &crate::layout::NodePath,
) -> Option<(u16, u16)> {
    use crate::layout::{NodeStep, SplitDir};
    let mut node = tree;
    let mut bounds = content;
    let mut steps = path.0.as_slice();
    loop {
        let LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } = node
        else {
            return None;
        };
        let (axis_start, axis_len) = match dir {
            SplitDir::Horizontal => (bounds.x, bounds.w),
            SplitDir::Vertical => (bounds.y, bounds.h),
            _ => return None,
        };
        match steps.split_first() {
            // The target split. Its content budget is the axis length
            // minus the reserved divider cell.
            None => {
                let content_len = axis_len.saturating_sub(1);
                if content_len == 0 {
                    return None;
                }
                return Some((axis_start, content_len));
            }
            // Descend, reproducing walk_layout's child-bounds math —
            // including §6.2 min-size freezing, so the span a drag
            // converts pointer cells against matches the divider the
            // frozen tiling actually painted.
            Some((step, rest)) => {
                steps = rest;
                let has_divider = axis_len >= 1;
                let content_len = axis_len.saturating_sub(1);
                let (min_low, min_high) = match dir {
                    SplitDir::Horizontal => (min_dims(left).0, min_dims(right).0),
                    SplitDir::Vertical => (min_dims(left).1, min_dims(right).1),
                    _ => return None,
                };
                let low = freeze_split_dim(content_len, *ratio, min_low, min_high);
                let high = content_len - low;
                let divider = axis_start + low;
                let (child, child_start, child_len) = match (dir, step) {
                    (SplitDir::Horizontal, NodeStep::Left) => (left, bounds.x, low),
                    (SplitDir::Horizontal, NodeStep::Right) => (
                        right,
                        if has_divider { divider + 1 } else { bounds.x },
                        high,
                    ),
                    (SplitDir::Vertical, NodeStep::Left) => (left, bounds.y, low),
                    (SplitDir::Vertical, NodeStep::Right) => (
                        right,
                        if has_divider { divider + 1 } else { bounds.y },
                        high,
                    ),
                    _ => return None,
                };
                node = child;
                bounds = match dir {
                    SplitDir::Horizontal => Rect {
                        x: child_start,
                        y: bounds.y,
                        w: child_len,
                        h: bounds.h,
                    },
                    SplitDir::Vertical => Rect {
                        x: bounds.x,
                        y: child_start,
                        w: bounds.w,
                        h: child_len,
                    },
                    _ => return None,
                };
            }
        }
    }
}

/// Tile leaf rects into `content`, an arbitrary sub-rectangle.
///
/// The reflow counterpart to [`compute_layout_in`] (and the inset analogue of
/// [`pane_rects`]), so an inset chrome like a sidebar sizes each pane's PTY to
/// the same rect it is painted into.
#[must_use]
pub fn pane_rects_in(tree: &LayoutNode, content: Rect) -> HashMap<TerminalId, Rect> {
    let mut segments: Vec<DividerSegment> = Vec::new();
    let mut rects: HashMap<TerminalId, Rect> = HashMap::new();
    walk_layout(tree, content, &mut segments, &mut rects);
    rects
}

/// [`pane_rects_in`] without §6.2 min-size freezing: the raw
/// proportional tiling of the tree's ratios.
///
/// This is what a split's `ratio` *asks for*, before freezing
/// redistributes space to hold squeezed leaves at their floor. The
/// ADR-0019 decision 5 resize gate checks candidate ratios against this
/// view: gating on the frozen rects would never trip on a frozen axis
/// (the floor pins the rect at minimum while the ratio drifts past it),
/// letting `resize-pane` bank unbounded ratio that the layout would
/// snap to on the next viewport grow. Paint and reflow must keep using
/// the frozen [`pane_rects_in`].
#[must_use]
pub fn pane_rects_proportional_in(tree: &LayoutNode, content: Rect) -> HashMap<TerminalId, Rect> {
    let mut segments: Vec<DividerSegment> = Vec::new();
    let mut rects: HashMap<TerminalId, Rect> = HashMap::new();
    walk_layout_proportional(tree, content, &mut segments, &mut rects);
    rects
}
