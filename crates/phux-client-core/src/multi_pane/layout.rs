use std::collections::HashMap;

use phux_protocol::ResourceId;

use crate::layout::{LayoutNode, LayoutState, Rect};

use super::rasterize::{
    DividerCell, DividerHit, DividerSegment, divider_hits, freeze_split_dim, min_dims, rasterize,
    walk_layout, walk_layout_proportional,
};

/// Result of [`compute_layout`]: per-pane rectangles plus divider cells,
/// together tiling the viewport exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneLayout {
    /// The outer viewport this layout was computed against.
    pub viewport: (u16, u16),
    /// Per-pane bounding rectangle, in outer-viewport cell coordinates.
    pub rects: HashMap<ResourceId, Rect>,
    /// Divider cells with their resolved box-drawing character.
    pub dividers: Vec<DividerCell>,
    /// Per-split grab targets for drag-to-resize (ADR-0048); every hit cell
    /// is a painted divider cell and vice versa.
    pub divider_hits: Vec<DividerHit>,
}

/// Compute per-pane rectangles and divider cells for `layout` inside a
/// `viewport_dims` viewport; empty when there is no tree or no area.
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

/// [`compute_layout`] tiled into `content`, a sub-rectangle left after
/// chrome insets. Dividers still clamp against the full `viewport_dims`.
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

    // One walk: leaf rects plus one divider segment per interior split.
    let mut segments: Vec<DividerSegment> = Vec::new();
    let mut rects: HashMap<ResourceId, Rect> = HashMap::new();
    walk_layout(tree, content, &mut segments, &mut rects);

    // Clamp to the full viewport: segments already carry inset coordinates.
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

/// Per-leaf rectangles for `tree` in a `viewport_dims` viewport: the same
/// walk [`compute_layout`] paints with, so reflow sizes each PTY to exactly
/// the rect it is painted into. Every leaf gets a rect (possibly empty).
#[must_use]
pub fn pane_rects(tree: &LayoutNode, viewport_dims: (u16, u16)) -> HashMap<ResourceId, Rect> {
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

/// The `(start, content_len)` span the split at `path` divides by its
/// ratio, along its axis in viewport coordinates (ADR-0048); a drag maps
/// pointer cell `p` to `(p - start) / content_len`. `None` when `path` is
/// not a split or the budget is zero.
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
            // Reproduce walk_layout's frozen child bounds.
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

/// [`pane_rects`] tiled into `content`, the reflow twin of
/// [`compute_layout_in`].
#[must_use]
pub fn pane_rects_in(tree: &LayoutNode, content: Rect) -> HashMap<ResourceId, Rect> {
    let mut segments: Vec<DividerSegment> = Vec::new();
    let mut rects: HashMap<ResourceId, Rect> = HashMap::new();
    walk_layout(tree, content, &mut segments, &mut rects);
    rects
}

/// [`pane_rects_in`] without min-size freezing: what the ratios ask for.
/// The resize gate checks this view, since frozen rects never trip it;
/// paint and reflow use the frozen [`pane_rects_in`].
#[must_use]
pub fn pane_rects_proportional_in(tree: &LayoutNode, content: Rect) -> HashMap<ResourceId, Rect> {
    let mut segments: Vec<DividerSegment> = Vec::new();
    let mut rects: HashMap<ResourceId, Rect> = HashMap::new();
    walk_layout_proportional(tree, content, &mut segments, &mut rects);
    rects
}
