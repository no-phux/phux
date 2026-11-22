//! The abstract shapes every encoding is built from and projected back to.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Axis {
    /// Two rectangles side by side, divided by a vertical bar.
    SideBySide,
    /// Two rectangles stacked, divided by a horizontal bar.
    Stacked,
}

#[derive(Debug, Clone)]
pub(super) enum Shape {
    /// A single pane, identified by its corpus-local index.
    Pane(u32),
    /// An interior split. `first` is the left/top child — the one `ratio`
    /// describes.
    Divide {
        axis: Axis,
        ratio: f32,
        first: Box<Self>,
        second: Box<Self>,
    },
}

/// Bit-exact on `ratio`: a projection that rounds, negates a zero, or turns a
/// value into a NaN must not compare equal to the shape it came from.
impl PartialEq for Shape {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Pane(a), Self::Pane(b)) => a == b,
            (
                Self::Divide {
                    axis: axis_a,
                    ratio: ratio_a,
                    first: first_a,
                    second: second_a,
                },
                Self::Divide {
                    axis: axis_b,
                    ratio: ratio_b,
                    first: first_b,
                    second: second_b,
                },
            ) => {
                axis_a == axis_b
                    && ratio_a.to_bits() == ratio_b.to_bits()
                    && first_a == first_b
                    && second_a == second_b
            }
            _ => false,
        }
    }
}

impl Shape {
    pub(super) fn split(axis: Axis, ratio: f32, first: Self, second: Self) -> Self {
        Self::Divide {
            axis,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    /// Pane indices in left-to-right traversal order.
    pub(super) fn panes(&self) -> Vec<u32> {
        let mut out = Vec::new();
        self.collect_panes(&mut out);
        out
    }

    fn collect_panes(&self, out: &mut Vec<u32>) {
        match self {
            Self::Pane(i) => out.push(*i),
            Self::Divide { first, second, .. } => {
                first.collect_panes(out);
                second.collect_panes(out);
            }
        }
    }
}

/// Panes in the deep-spine entry: each new pane nests one level down the
/// left spine, as an older image's registry built a window, so this is a
/// 64-deep tree, past `serde_json`'s default recursion limit for the upgrade
/// blob.
pub(super) const DEEP_SPINE_PANES: usize = 64;

/// One shape per interesting structural or numeric case. Every entry is fed
/// through all three encodings.
pub(super) fn corpus() -> Vec<(&'static str, Shape)> {
    vec![
        ("single leaf", Shape::Pane(0)),
        (
            "one side-by-side split",
            Shape::split(Axis::SideBySide, 0.5, Shape::Pane(0), Shape::Pane(1)),
        ),
        (
            "one stacked split",
            Shape::split(Axis::Stacked, 0.5, Shape::Pane(0), Shape::Pane(1)),
        ),
        (
            "nested on both axes, asymmetric",
            Shape::split(
                Axis::SideBySide,
                0.25,
                Shape::split(Axis::Stacked, 0.75, Shape::Pane(0), Shape::Pane(1)),
                Shape::split(
                    Axis::SideBySide,
                    0.5,
                    Shape::Pane(2),
                    Shape::split(Axis::Stacked, 0.125, Shape::Pane(3), Shape::Pane(4)),
                ),
            ),
        ),
        (
            "ratio at the exact boundaries",
            Shape::split(
                Axis::SideBySide,
                0.0,
                Shape::Pane(0),
                Shape::split(Axis::Stacked, 1.0, Shape::Pane(1), Shape::Pane(2)),
            ),
        ),
        (
            "ratio just inside the boundaries",
            Shape::split(
                Axis::Stacked,
                f32::EPSILON,
                Shape::Pane(0),
                Shape::split(
                    Axis::SideBySide,
                    1.0 - f32::EPSILON,
                    Shape::Pane(1),
                    Shape::Pane(2),
                ),
            ),
        ),
        (
            "deep asymmetric left spine",
            registry_spine(DEEP_SPINE_PANES),
        ),
        ("balanced tree, depth 6", balanced(6, &mut 0)),
    ]
}

/// The tree an older image's registry built for `panes` panes in one
/// window: the active pane 0 is split each time, so pane 1 is the outermost
/// right child. Repeated [`LayoutNode::split`] of pane 0 builds the same
/// shape ([`core_split_builds_the_registry_spine`]).
pub(super) fn registry_spine(panes: usize) -> Shape {
    let mut node = Shape::Pane(0);
    for i in (1..u32::try_from(panes).expect("corpus is small")).rev() {
        node = Shape::split(Axis::SideBySide, 0.5, node, Shape::Pane(i));
    }
    node
}

/// A fully balanced tree of `depth` splits, alternating axis by level.
pub(super) fn balanced(depth: u32, next: &mut u32) -> Shape {
    if depth == 0 {
        let leaf = Shape::Pane(*next);
        *next += 1;
        return leaf;
    }
    let axis = if depth.is_multiple_of(2) {
        Axis::SideBySide
    } else {
        Axis::Stacked
    };
    let first = balanced(depth - 1, next);
    let second = balanced(depth - 1, next);
    Shape::split(axis, 0.5, first, second)
}
