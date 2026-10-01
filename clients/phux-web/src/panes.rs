//! Bounded, client-local split geometry; resource lifetimes remain server-owned.

use phux_protocol::ids::ResourceId;

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

pub(crate) enum Layout {
    Leaf(ResourceId),
    Split(Axis, Box<Self>, Box<Self>),
}

impl Layout {
    pub(crate) fn insert(&mut self, target: &ResourceId, id: ResourceId, axis: Axis) -> bool {
        match self {
            Self::Leaf(old) if old == target => {
                *self = Self::Split(
                    axis,
                    Box::new(Self::Leaf(old.clone())),
                    Box::new(Self::Leaf(id)),
                );
                true
            }
            Self::Leaf(_) => false,
            Self::Split(_, first, second) => {
                first.insert(target, id.clone(), axis) || second.insert(target, id, axis)
            }
        }
    }

    pub(crate) fn remove(self, id: &ResourceId) -> Option<Self> {
        match self {
            Self::Leaf(old) => (old != *id).then_some(Self::Leaf(old)),
            Self::Split(axis, first, second) => match (first.remove(id), second.remove(id)) {
                (Some(first), Some(second)) => {
                    Some(Self::Split(axis, Box::new(first), Box::new(second)))
                }
                (remaining, None) | (None, remaining) => remaining,
            },
        }
    }

    pub(crate) fn rects(&self, rect: PaneRect, out: &mut Vec<(ResourceId, PaneRect)>) {
        match self {
            Self::Leaf(id) => out.push((id.clone(), rect)),
            Self::Split(axis, first, second) => {
                // A window can become smaller than the tree's minimum. Preserve
                // every pane and clip it rather than dropping a live resource.
                let (a, b) = rect.split(*axis).unwrap_or_else(|| {
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
                first.rects(a, out);
                second.rects(b, out);
            }
        }
    }
}
