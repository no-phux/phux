//! Layout-tree operations on [`Window`], using real ids from a [`Registry`].

#![allow(clippy::expect_used, clippy::unwrap_used)]

use phux_core::{LayoutError, LayoutNode, Registry, ResourceId, SplitDir, WindowId};

/// A registry with one window whose layout is reset to `Leaf(first)`, plus
/// `extra` further terminal ids to split with.
fn window_with(extra: usize) -> (Registry, WindowId, Vec<ResourceId>) {
    let mut reg = Registry::new();
    let s = reg.new_session("test".to_owned());
    let w = reg.new_window(s).expect("session exists");
    let ids: Vec<ResourceId> = (0..=extra)
        .map(|_| reg.new_terminal(w).expect("window exists"))
        .collect();
    reg.window_mut(w).expect("window exists").layout = Some(LayoutNode::Leaf(ids[0]));
    (reg, w, ids)
}

fn split(left: LayoutNode, right: LayoutNode, dir: SplitDir) -> LayoutNode {
    LayoutNode::Split {
        dir,
        ratio: 0.5,
        left: Box::new(left),
        right: Box::new(right),
    }
}

#[test]
fn nested_split_rewrites_only_the_target_leaf() {
    let (mut reg, w, ids) = window_with(2);
    let (p1, p2, p3) = (ids[0], ids[1], ids[2]);
    let win = reg.window_mut(w).expect("window exists");
    win.split(p1, p2, SplitDir::Horizontal, 0.5)
        .expect("split p1");
    win.split(p2, p3, SplitDir::Vertical, 0.5)
        .expect("split p2");
    assert_eq!(
        win.layout,
        Some(split(
            LayoutNode::Leaf(p1),
            split(
                LayoutNode::Leaf(p2),
                LayoutNode::Leaf(p3),
                SplitDir::Vertical
            ),
            SplitDir::Horizontal,
        ))
    );
}

#[test]
fn split_rejects_bad_input_without_changing_layout() {
    let (mut reg, w, ids) = window_with(2);
    let (p1, missing, new_pane) = (ids[0], ids[1], ids[2]);
    let win = reg.window_mut(w).expect("window exists");
    let original = win.layout.clone();

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
                win.split(p1, new_pane, SplitDir::Horizontal, ratio),
                Err(LayoutError::InvalidRatio(_))
            ),
            "split must reject ratio {ratio}"
        );
        assert_eq!(
            win.layout, original,
            "rejected ratio {ratio} changed the layout"
        );
    }
    assert_eq!(
        win.split(missing, new_pane, SplitDir::Horizontal, 0.5),
        Err(LayoutError::PaneNotInLayout(missing))
    );
    assert_eq!(win.layout, original);
}

#[test]
fn kill_pane_collapses_the_parent_split_and_reports_edge_cases() {
    let (mut reg, w, ids) = window_with(1);
    let (p1, p2) = (ids[0], ids[1]);
    let win = reg.window_mut(w).expect("window exists");
    win.split(p1, p2, SplitDir::Horizontal, 0.5)
        .expect("split p1");

    let bogus = ResourceId::default();
    assert_eq!(
        win.kill_pane(bogus),
        Err(LayoutError::PaneNotInLayout(bogus))
    );

    win.kill_pane(p1).expect("kill p1");
    assert_eq!(win.layout, Some(LayoutNode::Leaf(p2)));

    assert_eq!(win.kill_pane(p2), Err(LayoutError::LastPane));
    assert_eq!(win.layout, None);
}
