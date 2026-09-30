use std::collections::HashMap;

use phux_protocol::ResourceId;

use crate::layout::{LayoutNode, NodePath, NodeStep, Rect, SplitDir};

/// One cell of the divider grid, with its resolved box-drawing glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DividerCell {
    /// Column in outer-viewport coordinates.
    pub x: u16,
    /// Row in outer-viewport coordinates.
    pub y: u16,
    /// The box-drawing character, always from the light set: mixed-weight
    /// junctions are missing or misdrawn in most terminal fonts.
    pub ch: char,
}

/// A grab target: the cells of one split's divider line, its node path,
/// and its axis (`Horizontal` paints a vertical line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DividerHit {
    /// Path from the layout root to the controlling [`LayoutNode::Split`].
    pub node_path: NodePath,
    /// The split's axis (`dir`). Drives whether a drag reads the
    /// pointer's x (Horizontal) or y (Vertical).
    pub axis: SplitDir,
    /// Outer-viewport cells the divider line occupies, in long-axis order.
    pub cells: Vec<(u16, u16)>,
}

/// One divider segment: either a vertical line (from a Horizontal
/// split) or a horizontal line (from a Vertical split), in
/// outer-viewport cell coordinates.
#[derive(Debug, Clone)]
pub(super) struct DividerSegment {
    /// Direction of the *line itself*: a Horizontal split produces a
    /// vertical line; we tag the segment with the split's dir so the
    /// rasterizer knows which axis to walk.
    split: SplitDir,
    /// Inclusive cell range along the segment's long axis.
    a0: u16,
    /// Inclusive cell range along the segment's long axis.
    a1: u16,
    /// Cell index on the perpendicular (cross) axis.
    cross: u16,
    /// Path to the split this segment divides.
    node_path: NodePath,
}

/// Recursively split `bounds` by the tree, recording one divider segment
/// per split and a rect for every leaf (possibly empty), in viewport
/// coordinates.
///
/// Leaf rects plus divider cells tile `bounds` exactly. Each cut is
/// clamped so both sides keep their aggregate minimums ([`MIN_LEAF_COLS`]
/// x [`MIN_LEAF_ROWS`] per leaf); when `bounds` cannot fit them the clamp
/// disengages and proportional tiling resumes.
pub(super) fn walk_layout(
    node: &LayoutNode,
    bounds: Rect,
    segments: &mut Vec<DividerSegment>,
    rects: &mut HashMap<ResourceId, Rect>,
) {
    walk_layout_at(node, bounds, &mut NodePath::root(), segments, rects, true);
}

/// [`walk_layout`] without min-size freezing (see [`crate::multi_pane::pane_rects_proportional_in`]).
pub(super) fn walk_layout_proportional(
    node: &LayoutNode,
    bounds: Rect,
    segments: &mut Vec<DividerSegment>,
    rects: &mut HashMap<ResourceId, Rect>,
) {
    walk_layout_at(node, bounds, &mut NodePath::root(), segments, rects, false);
}

/// [`walk_layout`] with the root-to-`node` `path`; `freeze` selects
/// min-size freezing.
#[allow(
    clippy::too_many_lines,
    reason = "the Horizontal and Vertical arms are near-mirror child-bounds math; splitting them loses the side-by-side readability that makes the divider-reservation symmetry auditable."
)]
fn walk_layout_at(
    node: &LayoutNode,
    bounds: Rect,
    path: &mut NodePath,
    segments: &mut Vec<DividerSegment>,
    rects: &mut HashMap<ResourceId, Rect>,
    freeze: bool,
) {
    match node {
        LayoutNode::Leaf(p) => {
            rects.insert(p.clone(), bounds);
        }
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => match dir {
            SplitDir::Horizontal => {
                // Reserve one column for the divider only when there is
                // width to spare. At `bounds.w == 0` the subtree is
                // invisible: no divider, both children get zero width.
                let has_divider = bounds.w >= 1;
                let content_w = bounds.w.saturating_sub(1);
                let left_w = if freeze {
                    freeze_split_dim(content_w, *ratio, min_dims(left).0, min_dims(right).0)
                } else {
                    split_dim(content_w, *ratio)
                };
                let right_w = content_w - left_w;
                let divider_x = bounds.x + left_w;
                if has_divider {
                    segments.push(DividerSegment {
                        split: SplitDir::Horizontal,
                        a0: bounds.y,
                        a1: bounds.y + bounds.h.saturating_sub(1),
                        cross: divider_x,
                        node_path: path.clone(),
                    });
                }
                path.push(NodeStep::Left);
                walk_layout_at(
                    left,
                    Rect {
                        x: bounds.x,
                        y: bounds.y,
                        w: left_w,
                        h: bounds.h,
                    },
                    path,
                    segments,
                    rects,
                    freeze,
                );
                path.pop();
                path.push(NodeStep::Right);
                walk_layout_at(
                    right,
                    Rect {
                        x: if has_divider { divider_x + 1 } else { bounds.x },
                        y: bounds.y,
                        w: right_w,
                        h: bounds.h,
                    },
                    path,
                    segments,
                    rects,
                    freeze,
                );
                path.pop();
            }
            SplitDir::Vertical => {
                let has_divider = bounds.h >= 1;
                let content_h = bounds.h.saturating_sub(1);
                let top_h = if freeze {
                    freeze_split_dim(content_h, *ratio, min_dims(left).1, min_dims(right).1)
                } else {
                    split_dim(content_h, *ratio)
                };
                let bot_h = content_h - top_h;
                let divider_y = bounds.y + top_h;
                if has_divider {
                    segments.push(DividerSegment {
                        split: SplitDir::Vertical,
                        a0: bounds.x,
                        a1: bounds.x + bounds.w.saturating_sub(1),
                        cross: divider_y,
                        node_path: path.clone(),
                    });
                }
                path.push(NodeStep::Left);
                walk_layout_at(
                    left,
                    Rect {
                        x: bounds.x,
                        y: bounds.y,
                        w: bounds.w,
                        h: top_h,
                    },
                    path,
                    segments,
                    rects,
                    freeze,
                );
                path.pop();
                path.push(NodeStep::Right);
                walk_layout_at(
                    right,
                    Rect {
                        x: bounds.x,
                        y: if has_divider { divider_y + 1 } else { bounds.y },
                        w: bounds.w,
                        h: bot_h,
                    },
                    path,
                    segments,
                    rects,
                    freeze,
                );
                path.pop();
            }
        },
    }
}

/// Per-cell record of the divider grid: which edges are present at one
/// coordinate. Order is [N, E, S, W].
#[derive(Default, Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the four fields ARE the four cardinal directions; a bitflag or an array would only obscure which edge is which at every use site"
)]
struct DividerEdges {
    north: bool,
    east: bool,
    south: bool,
    west: bool,
}

/// The four cardinal neighbours of one grid coordinate. A direction is
/// `Some` only when that neighbour is itself a divider cell.
#[derive(Default, Clone, Copy)]
struct Neighbours {
    north: Option<DividerEdges>,
    east: Option<DividerEdges>,
    south: Option<DividerEdges>,
    west: Option<DividerEdges>,
}

impl Neighbours {
    /// Read the divider cells cardinally adjacent to `(x, y)`.
    fn around(grid: &HashMap<(u16, u16), DividerEdges>, x: u16, y: u16) -> Self {
        Self {
            north: if y > 0 {
                grid.get(&(x, y - 1)).copied()
            } else {
                None
            },
            east: grid.get(&(x + 1, y)).copied(),
            south: grid.get(&(x, y + 1)).copied(),
            west: if x > 0 {
                grid.get(&(x - 1, y)).copied()
            } else {
                None
            },
        }
    }
}

impl DividerEdges {
    /// Inherit a junction edge toward each cardinal neighbour that is a
    /// divider cell and that we don't already have an edge toward. The
    /// inherited weight matches the neighbour's touching edge.
    const fn inherit_junctions(&mut self, neighbours: &Neighbours) {
        if !self.north {
            self.north = neighbours.north.is_some();
        }
        if !self.south {
            self.south = neighbours.south.is_some();
        }
        if !self.east {
            self.east = neighbours.east.is_some();
        }
        if !self.west {
            self.west = neighbours.west.is_some();
        }
    }
}

/// Lay down a vertical line at column `cross` from row a0..=a1.
fn lay_down_vertical_line(
    grid: &mut HashMap<(u16, u16), DividerEdges>,
    seg: &DividerSegment,
    vcols: u16,
    vrows: u16,
) {
    let x = seg.cross;
    if x >= vcols {
        return;
    }
    for y in seg.a0..=seg.a1.min(vrows.saturating_sub(1)) {
        let cell = grid.entry((x, y)).or_default();
        if y > seg.a0 {
            cell.north = true;
        }
        if y < seg.a1 {
            cell.south = true;
        }
    }
}

/// Lay down a horizontal line at row `cross` from col a0..=a1.
fn lay_down_horizontal_line(
    grid: &mut HashMap<(u16, u16), DividerEdges>,
    seg: &DividerSegment,
    vcols: u16,
    vrows: u16,
) {
    let y = seg.cross;
    if y >= vrows {
        return;
    }
    for x in seg.a0..=seg.a1.min(vcols.saturating_sub(1)) {
        let cell = grid.entry((x, y)).or_default();
        if x > seg.a0 {
            cell.west = true;
        }
        if x < seg.a1 {
            cell.east = true;
        }
    }
}

/// Lay down one segment's cells, recording incident edges. A
/// `Horizontal` split paints a vertical line, a `Vertical` split a
/// horizontal one.
fn lay_down_segment(
    grid: &mut HashMap<(u16, u16), DividerEdges>,
    seg: &DividerSegment,
    viewport: (u16, u16),
) {
    let (vcols, vrows) = viewport;
    match seg.split {
        SplitDir::Horizontal => lay_down_vertical_line(grid, seg, vcols, vrows),
        SplitDir::Vertical => lay_down_horizontal_line(grid, seg, vcols, vrows),
    }
}

/// Post-pass: give a cell an edge toward each neighbouring divider cell,
/// so a segment ending against another paints a T-junction.
fn inherit_junction_edges(grid: &mut HashMap<(u16, u16), DividerEdges>) {
    let cell_coords: Vec<(u16, u16)> = grid.keys().copied().collect();
    for (x, y) in cell_coords {
        let neighbours = Neighbours::around(grid, x, y);
        let Some(cell) = grid.get_mut(&(x, y)) else {
            continue;
        };
        cell.inherit_junctions(&neighbours);
    }
}

/// Resolve every grid cell to its box-drawing glyph, in stable
/// row-major output order.
fn into_divider_cells(grid: &HashMap<(u16, u16), DividerEdges>) -> Vec<DividerCell> {
    let mut keys: Vec<(u16, u16)> = grid.keys().copied().collect();
    keys.sort_by_key(|(x, y)| (*y, *x));
    keys.into_iter()
        .map(|(x, y)| {
            let cell = grid[&(x, y)];
            DividerCell {
                x,
                y,
                ch: pick_box_char(cell),
            }
        })
        .collect()
}

/// Convert segments into per-cell divider glyphs, resolving junctions.
pub(super) fn rasterize(segments: &[DividerSegment], viewport: (u16, u16)) -> Vec<DividerCell> {
    let mut grid: HashMap<(u16, u16), DividerEdges> = HashMap::new();
    for seg in segments {
        lay_down_segment(&mut grid, seg, viewport);
    }
    inherit_junction_edges(&mut grid);
    into_divider_cells(&grid)
}

/// Build the per-split grab map from the same segments and viewport clamp
/// as [`rasterize`], so hit cells are exactly painted cells.
pub(super) fn divider_hits(segments: &[DividerSegment], viewport: (u16, u16)) -> Vec<DividerHit> {
    let (vcols, vrows) = viewport;
    segments
        .iter()
        .map(|seg| {
            let mut cells = Vec::new();
            match seg.split {
                SplitDir::Horizontal => {
                    // Vertical line at column `cross` from row a0..=a1.
                    let x = seg.cross;
                    if x < vcols {
                        for y in seg.a0..=seg.a1.min(vrows.saturating_sub(1)) {
                            cells.push((x, y));
                        }
                    }
                }
                SplitDir::Vertical => {
                    // Horizontal line at row `cross` from col a0..=a1.
                    let y = seg.cross;
                    if y < vrows {
                        for x in seg.a0..=seg.a1.min(vcols.saturating_sub(1)) {
                            cells.push((x, y));
                        }
                    }
                }
            }
            DividerHit {
                node_path: seg.node_path.clone(),
                axis: seg.split,
                cells,
            }
        })
        .collect()
}

/// Pick the light box-drawing character for a cell's present edges.
const fn pick_box_char(edges: DividerEdges) -> char {
    match (edges.north, edges.east, edges.south, edges.west) {
        // No incident edge: nothing to draw.
        (false, false, false, false) => ' ',
        // Four-way cross.
        (true, true, true, true) => '\u{253C}', // ┼
        // T-pieces.
        (true, false, true, true) => '\u{2524}', // ┤
        (true, true, true, false) => '\u{251C}', // ├
        (true, true, false, true) => '\u{2534}', // ┴
        (false, true, true, true) => '\u{252C}', // ┬
        // Corners.
        (false, true, true, false) => '\u{250C}', // ┌
        (false, false, true, true) => '\u{2510}', // ┐
        (true, true, false, false) => '\u{2514}', // └
        (true, false, false, true) => '\u{2518}', // ┘
        // Nothing east or west: a vertical run, or a stub continuing one.
        (_, false, _, false) => '\u{2502}', // │
        // Nothing north or south: a horizontal run, or a stub of one.
        (false, _, false, _) => '\u{2500}', // ─
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub(super) fn split_dim(total: u16, ratio: f32) -> u16 {
    // Mirror crate::layout::split_dim (private there) for divider math.
    let raw = (f32::from(total) * ratio).round();
    if raw < 0.0 {
        0
    } else if raw > f32::from(total) {
        total
    } else {
        raw as u16
    }
}

/// Minimum inner-content width of a leaf pane, in cells (TUI doc §6.2).
pub(super) const MIN_LEAF_COLS: u16 = 2;

/// Minimum inner-content height of a leaf pane, in cells (TUI doc §6.2).
pub(super) const MIN_LEAF_ROWS: u16 = 1;

/// The smallest `(cols, rows)` giving every leaf of `node` its minimum.
pub(super) fn min_dims(node: &LayoutNode) -> (u16, u16) {
    match node {
        LayoutNode::Leaf(_) => (MIN_LEAF_COLS, MIN_LEAF_ROWS),
        LayoutNode::Split {
            dir, left, right, ..
        } => {
            let (lw, lh) = min_dims(left);
            let (rw, rh) = min_dims(right);
            match dir {
                SplitDir::Horizontal => (lw.saturating_add(rw).saturating_add(1), lh.max(rh)),
                SplitDir::Vertical => (lw.max(rw), lh.saturating_add(rh).saturating_add(1)),
            }
        }
    }
}

/// [`split_dim`] clamped so each side keeps its minimum; unclamped when
/// `content` cannot cover both.
pub(super) fn freeze_split_dim(content: u16, ratio: f32, min_low: u16, min_high: u16) -> u16 {
    let low = split_dim(content, ratio);
    match min_low.checked_add(min_high) {
        Some(needed) if content >= needed => low.clamp(min_low, content - min_high),
        _ => low,
    }
}
