//! Split-tree operations on [`LayoutNode<u32>`].

#![allow(clippy::expect_used, clippy::unwrap_used)]

use phux_core::arrangement::{collapse, remove};
use phux_core::{LayoutError, LayoutNode, SplitDir};

fn split(left: LayoutNode<u32>, right: LayoutNode<u32>, dir: SplitDir) -> LayoutNode<u32> {
    LayoutNode::Split {
        dir,
        ratio: 0.5,
        left: Box::new(left),
        right: Box::new(right),
    }
}

#[test]
fn nested_split_rewrites_only_the_target_leaf() {
    let mut tree = LayoutNode::Leaf(1);
    tree.split(&1, 2, SplitDir::Horizontal, 0.5)
        .expect("split 1");
    tree.split(&2, 3, SplitDir::Vertical, 0.5).expect("split 2");
    assert_eq!(
        tree,
        split(
            LayoutNode::Leaf(1),
            split(LayoutNode::Leaf(2), LayoutNode::Leaf(3), SplitDir::Vertical),
            SplitDir::Horizontal,
        )
    );
    assert_eq!(tree.leaves(), vec![&1, &2, &3]);
}

#[test]
fn split_rejects_bad_input_without_changing_layout() {
    let mut tree = LayoutNode::Leaf(1);
    let original = tree.clone();

    for ratio in [
        f32::NEG_INFINITY,
        -0.5,
        -0.0,
        0.0,
        1.0,
        1.5,
        f32::INFINITY,
        f32::NAN,
    ] {
        assert!(
            matches!(
                tree.split(&1, 3, SplitDir::Horizontal, ratio),
                Err(LayoutError::InvalidRatio(_))
            ),
            "split must reject ratio {ratio}"
        );
        assert_eq!(tree, original, "rejected ratio {ratio} changed the layout");
    }
    assert_eq!(
        tree.split(&2, 3, SplitDir::Horizontal, 0.5),
        Err(LayoutError::PaneNotInLayout(2))
    );
    assert_eq!(tree, original);
}

#[test]
fn remove_collapses_the_parent_split_and_empties_on_the_last_leaf() {
    let mut tree = Some(split(
        LayoutNode::Leaf(1),
        LayoutNode::Leaf(2),
        SplitDir::Horizontal,
    ));
    assert_eq!(remove(&mut tree, &9), Err(LayoutError::PaneNotInLayout(9)));

    remove(&mut tree, &1).expect("remove 1");
    assert_eq!(tree, Some(LayoutNode::Leaf(2)));

    remove(&mut tree, &2).expect("remove the last leaf");
    assert_eq!(tree, None);
    assert_eq!(remove(&mut tree, &2), Err(LayoutError::PaneNotInLayout(2)));
}

#[test]
fn collapse_drops_every_matching_leaf_and_keeps_shape() {
    let tree = split(
        split(LayoutNode::Leaf(1), LayoutNode::Leaf(2), SplitDir::Vertical),
        split(LayoutNode::Leaf(3), LayoutNode::Leaf(4), SplitDir::Vertical),
        SplitDir::Horizontal,
    );
    let kept = collapse(tree.clone(), &|l| l % 2 == 0).expect("odd leaves survive");
    assert_eq!(
        kept,
        split(
            LayoutNode::Leaf(1),
            LayoutNode::Leaf(3),
            SplitDir::Horizontal
        )
    );
    assert_eq!(collapse(tree, &|_| true), None);
}

#[test]
fn map_translates_leaves_and_validate_ratios_checks_hand_built_trees() {
    let tree = split(LayoutNode::Leaf(1), LayoutNode::Leaf(2), SplitDir::Vertical);
    let mapped = tree.map(&mut |l| format!("pane-{l}"));
    assert_eq!(mapped.leaves(), vec!["pane-1", "pane-2"]);
    let back: Result<LayoutNode<u32>, ()> = mapped.try_map(&mut |s| {
        s.strip_prefix("pane-")
            .and_then(|n| n.parse().ok())
            .ok_or(())
    });
    assert_eq!(back, Ok(tree.clone()));
    assert_eq!(tree.validate_ratios(), Ok(()));

    let bad = LayoutNode::Split {
        dir: SplitDir::Horizontal,
        ratio: 1.0,
        left: Box::new(LayoutNode::Leaf(1)),
        right: Box::new(LayoutNode::Leaf(2)),
    };
    assert_eq!(bad.validate_ratios(), Err(LayoutError::InvalidRatio(1.0)));
}
