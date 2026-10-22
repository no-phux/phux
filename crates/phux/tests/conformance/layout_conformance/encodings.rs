//! Build each corpus shape into the three encodings and project it back.

use std::collections::HashMap;

use phux_client::layout::{LayoutNode as L3Node, SplitDir as L3Dir};
use phux_core::arrangement::{LayoutNode, SplitDir as CoreDir};
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_server::upgrade::blob::{LayoutBlob, SplitDirBlob};

use super::corpus::{Axis, Shape};

/// The core tree with the corpus pane index as its leaf.
pub(super) type CoreNode = LayoutNode<u32>;

pub(super) fn build_core(shape: &Shape) -> CoreNode {
    match shape {
        Shape::Pane(i) => CoreNode::Leaf(*i),
        Shape::Divide {
            axis,
            ratio,
            first,
            second,
        } => CoreNode::Split {
            dir: match axis {
                Axis::SideBySide => CoreDir::Horizontal,
                Axis::Stacked => CoreDir::Vertical,
            },
            ratio: *ratio,
            left: Box::new(build_core(first)),
            right: Box::new(build_core(second)),
        },
    }
}

pub(super) fn project_core(node: &CoreNode) -> Shape {
    match node {
        CoreNode::Leaf(index) => Shape::Pane(*index),
        CoreNode::Split {
            dir,
            ratio,
            left,
            right,
        } => Shape::Divide {
            axis: match dir {
                CoreDir::Horizontal => Axis::SideBySide,
                CoreDir::Vertical => Axis::Stacked,
            },
            ratio: *ratio,
            first: Box::new(project_core(left)),
            second: Box::new(project_core(right)),
        },
    }
}

pub(super) const fn l3_pane_id(index: u32) -> WireResourceId {
    WireResourceId::new(index + 1)
}

pub(super) fn build_l3(shape: &Shape) -> L3Node {
    match shape {
        Shape::Pane(i) => L3Node::Leaf(l3_pane_id(*i)),
        Shape::Divide {
            axis,
            ratio,
            first,
            second,
        } => L3Node::Split {
            dir: match axis {
                Axis::SideBySide => L3Dir::Horizontal,
                Axis::Stacked => L3Dir::Vertical,
            },
            ratio: *ratio,
            left: Box::new(build_l3(first)),
            right: Box::new(build_l3(second)),
        },
    }
}

pub(super) fn project_l3(node: &L3Node) -> Shape {
    match node {
        L3Node::Leaf(tid) => Shape::Pane(
            tid.local_id()
                .expect("corpus builds Local ids only")
                .checked_sub(1)
                .expect("L3 pane ids start at 1"),
        ),
        L3Node::Split {
            dir,
            ratio,
            left,
            right,
        } => Shape::Divide {
            axis: match dir {
                L3Dir::Horizontal => Axis::SideBySide,
                L3Dir::Vertical => Axis::Stacked,
            },
            ratio: *ratio,
            first: Box::new(project_l3(left)),
            second: Box::new(project_l3(right)),
        },
    }
}

pub(super) fn build_blob(shape: &Shape) -> LayoutBlob {
    match shape {
        Shape::Pane(i) => LayoutBlob::Leaf(i + 1),
        Shape::Divide {
            axis,
            ratio,
            first,
            second,
        } => LayoutBlob::Split {
            dir: match axis {
                Axis::SideBySide => SplitDirBlob::Horizontal,
                Axis::Stacked => SplitDirBlob::Vertical,
            },
            ratio: *ratio,
            left: Box::new(build_blob(first)),
            right: Box::new(build_blob(second)),
        },
    }
}

pub(super) fn project_blob(node: &LayoutBlob, index_of: &HashMap<u32, u32>) -> Shape {
    match node {
        LayoutBlob::Leaf(wire) => Shape::Pane(index_of[wire]),
        LayoutBlob::Split {
            dir,
            ratio,
            left,
            right,
        } => Shape::Divide {
            axis: match dir {
                SplitDirBlob::Horizontal => Axis::SideBySide,
                SplitDirBlob::Vertical => Axis::Stacked,
            },
            ratio: *ratio,
            first: Box::new(project_blob(left, index_of)),
            second: Box::new(project_blob(right, index_of)),
        },
    }
}

/// Identity map for the blob's `u32` keys, matching [`build_blob`].
pub(super) fn blob_index_map(shape: &Shape) -> HashMap<u32, u32> {
    shape.panes().into_iter().map(|i| (i + 1, i)).collect()
}
