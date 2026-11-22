//! Bounded, client-local split geometry; resource lifetimes remain server-owned.

use phux_client_core::layout::{LayoutNode, SplitDir, kill_pane, split_at};
use phux_protocol::ids::ResourceId;

/// The axis a browser split divides: `Vertical` sets panes side by side
/// (a vertical separator), `Horizontal` stacks them.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Axis {
    Vertical,
    Horizontal,
}

impl Axis {
    pub(crate) fn parse(axis: &str) -> Result<Self, String> {
        match axis {
            "vertical" => Ok(Self::Vertical),
            "horizontal" => Ok(Self::Horizontal),
            _ => Err("Split axis must be vertical or horizontal.".to_owned()),
        }
    }

    /// The split-tree direction drawing this separator.
    const fn split_dir(self) -> SplitDir {
        match self {
            Self::Vertical => SplitDir::Horizontal,
            Self::Horizontal => SplitDir::Vertical,
        }
    }

    /// The separator a split-tree direction draws.
    const fn of(dir: SplitDir) -> Self {
        match dir {
            SplitDir::Horizontal => Self::Vertical,
            SplitDir::Vertical => Self::Horizontal,
        }
    }
}

/// A pane's cell rectangle within the original canvas, excluding separators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneRect {
    /// Left column in the canvas.
    pub x: u16,
    /// Top row in the canvas.
    pub y: u16,
    /// Terminal width in cells.
    pub cols: u16,
    /// Terminal height in cells.
    pub rows: u16,
}

impl PaneRect {
    pub(crate) fn split(self, axis: Axis) -> Option<(Self, Self)> {
        let mut first = self;
        let mut second = self;
        match axis {
            Axis::Vertical => {
                if self.cols < 5 {
                    return None;
                }
                first.cols = (self.cols - 1) / 2;
                second.x += first.cols + 1;
                second.cols = self.cols - first.cols - 1;
            }
            Axis::Horizontal => {
                if self.rows < 5 {
                    return None;
                }
                first.rows = (self.rows - 1) / 2;
                second.y += first.rows + 1;
                second.rows = self.rows - first.rows - 1;
            }
        }
        Some((first, second))
    }
}

/// The browser's split tree. Every split is even; [`rects`] owns the
/// geometry, so the stored ratio is never read.
pub(crate) type Layout = LayoutNode;

const EVEN: f32 = 0.5;

/// Split `target`'s leaf, `id` second along `axis`; `false` (and `layout`
/// unchanged) when `target` is not a leaf.
pub(crate) fn insert(layout: &mut Layout, target: &ResourceId, id: ResourceId, axis: Axis) -> bool {
    let Ok(split) = split_at(layout, target, &id, axis.split_dir(), EVEN) else {
        return false;
    };
    *layout = split;
    true
}

/// `layout` without `id`'s leaf; `None` once no leaf is left.
pub(crate) fn remove(layout: Layout, id: &ResourceId) -> Option<Layout> {
    kill_pane(&layout, id).unwrap_or(Some(layout))
}

/// Each leaf's rectangle within `rect`, in leaf order.
pub(crate) fn rects(layout: &Layout, rect: PaneRect, out: &mut Vec<(ResourceId, PaneRect)>) {
    match layout {
        LayoutNode::Leaf(id) => out.push((id.clone(), rect)),
        LayoutNode::Split {
            dir, left, right, ..
        } => {
            let axis = Axis::of(*dir);
            // A window can become smaller than the tree's minimum. Preserve
            // every pane and clip it rather than dropping a live resource.
            let (a, b) = rect.split(axis).unwrap_or_else(|| {
                let mut a = rect;
                let mut b = rect;
                match axis {
                    Axis::Vertical => {
                        a.cols = (rect.cols / 2).max(1);
                        b.x += a.cols;
                        b.cols = rect.cols.saturating_sub(a.cols).max(1);
                    }
                    Axis::Horizontal => {
                        a.rows = (rect.rows / 2).max(1);
                        b.y += a.rows;
                        b.rows = rect.rows.saturating_sub(a.rows).max(1);
                    }
                }
                (a, b)
            });
            rects(left, a, out);
            rects(right, b, out);
        }
    }
}
