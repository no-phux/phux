//! Layout-tree unit tests for [`Window`].
//!
//! Uses [`Registry`] to bootstrap real `ResourceId`s, then exercises the
//! tree operations on the `Window` directly. The layout invariants live in
//! `tests/layout_proptest.rs`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::similar_names,
    clippy::doc_markdown
)]

use std::collections::HashSet;

use phux_core::{Direction, LayoutNode, Registry, ResourceId, SplitDir};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a Registry seeded with one session, one window, and one initial
/// pane. Returns the (window_id, pane_id) plus the Registry.
fn seeded() -> (Registry, phux_core::WindowId, ResourceId) {
    let mut reg = Registry::new();
    let s = reg.new_session("test".to_owned());
    let w = reg.new_window(s).expect("session exists");
    let p = reg.new_terminal(w).expect("window exists");
    (reg, w, p)
}

// ---------------------------------------------------------------------------
// Tree shape
//
// Pane-rect *tiling* is intentionally not tested here: phux-core carries no
// tiling walk (bead phux-nnjx). The canonical, divider-aware walk and its
// exact-tiling tests live in `phux-client-core`'s `multi_pane` module.
// ---------------------------------------------------------------------------

#[test]
fn single_pane_layout_is_a_leaf() {
    let (reg, w, p) = seeded();
    let win = reg.window(w).expect("window exists");
    assert_eq!(win.layout, Some(LayoutNode::Leaf(p)));
}

#[test]
fn split_replaces_the_target_leaf_with_a_split_node() {
    let (mut reg, w, p1) = seeded();
    // Allocate a second pane id by creating it then re-splitting at p1.
    let p2 = reg.new_terminal(w).expect("window exists");
    // Reset the layout to a known shape via the Window API: byc.1's Registry
    // auto-splits, but we want a controlled state.
    {
        let win = reg.window_mut(w).expect("window exists");
        win.layout = Some(LayoutNode::Leaf(p1));
        win.split(p1, p2, SplitDir::Horizontal, 0.5)
            .expect("split p1");
    }

    let win = reg.window(w).expect("window exists");
    assert_eq!(
        win.layout,
        Some(LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutNode::Leaf(p1)),
            right: Box::new(LayoutNode::Leaf(p2)),
        })
    );
}

#[test]
fn nested_split_descends_into_the_target_leaf() {
    let (mut reg, w, p1) = seeded();
    let p2 = reg.new_terminal(w).expect("window exists");
    let p3 = reg.new_terminal(w).expect("window exists");
    {
        let win = reg.window_mut(w).expect("window exists");
        win.layout = Some(LayoutNode::Leaf(p1));
        win.split(p1, p2, SplitDir::Horizontal, 0.5)
            .expect("split p1");
        // Splitting p2 must rewrite the *right* leaf only.
        win.split(p2, p3, SplitDir::Vertical, 0.5)
            .expect("split p2");
    }

    let win = reg.window(w).expect("window exists");
    assert_eq!(
        win.layout,
        Some(LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutNode::Leaf(p1)),
            right: Box::new(LayoutNode::Split {
                dir: SplitDir::Vertical,
                ratio: 0.5,
                left: Box::new(LayoutNode::Leaf(p2)),
                right: Box::new(LayoutNode::Leaf(p3)),
            }),
        })
    );
}

#[test]
fn split_rejects_invalid_ratios_without_changing_layout() {
    let (mut reg, w, p1) = seeded();
    let p2 = reg.new_terminal(w).expect("window exists");
    let win = reg.window_mut(w).expect("window exists");
    win.layout = Some(LayoutNode::Leaf(p1));
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
                win.split(p1, p2, SplitDir::Horizontal, ratio),
                Err(phux_core::LayoutError::InvalidRatio(_))
            ),
            "split must reject ratio {ratio}"
        );
        assert_eq!(
            win.layout, original,
            "rejected ratio {ratio} changed the layout"
        );
    }
}

#[test]
fn split_missing_target_preserves_the_existing_leaf() {
    let (mut reg, w, p1) = seeded();
    let missing = reg.new_terminal(w).expect("window exists");
    let new_pane = reg.new_terminal(w).expect("window exists");
    let win = reg.window_mut(w).expect("window exists");
    win.layout = Some(LayoutNode::Leaf(p1));

    assert_eq!(
        win.split(missing, new_pane, SplitDir::Horizontal, 0.5),
        Err(phux_core::LayoutError::PaneNotInLayout(missing))
    );
    assert_eq!(win.layout, Some(LayoutNode::Leaf(p1)));
}

// ---------------------------------------------------------------------------
// kill_pane
// ---------------------------------------------------------------------------

#[test]
fn kill_pane_collapses_parent_split() {
    let (mut reg, w, p1) = seeded();
    let p2 = reg.new_terminal(w).expect("window exists");
    {
        let win = reg.window_mut(w).expect("window exists");
        win.layout = Some(LayoutNode::Leaf(p1));
        win.split(p1, p2, SplitDir::Horizontal, 0.5)
            .expect("split p1");
        // Tree is Split { Leaf(p1), Leaf(p2) }. Kill p1; the surviving leaf
        // should collapse up — layout becomes Leaf(p2).
        win.kill_pane(p1).expect("kill p1");
    }

    let win = reg.window(w).expect("window exists");
    assert_eq!(win.layout, Some(LayoutNode::Leaf(p2)));
}

#[test]
fn kill_pane_not_in_layout_errors() {
    let (mut reg, w, p1) = seeded();
    let bogus = ResourceId::default();
    assert_ne!(bogus, p1);
    let win = reg.window_mut(w).expect("window exists");
    win.layout = Some(LayoutNode::Leaf(p1));
    let err = win.kill_pane(bogus).unwrap_err();
    assert_eq!(err, phux_core::LayoutError::PaneNotInLayout(bogus));
    assert_eq!(win.layout, Some(LayoutNode::Leaf(p1)));
}

#[test]
fn kill_last_pane_returns_last_pane_error() {
    let (mut reg, w, p1) = seeded();
    let win = reg.window_mut(w).expect("window exists");
    assert_eq!(win.layout, Some(LayoutNode::Leaf(p1)));
    let err = win.kill_pane(p1).unwrap_err();
    assert!(matches!(err, phux_core::LayoutError::LastPane));
}

// ---------------------------------------------------------------------------
// focus_direction
// ---------------------------------------------------------------------------

#[test]
fn focus_direction_across_balanced_grid() {
    // Build a 2x2 grid:
    //   Horizontal split at root:
    //     left  = Vertical { top: p_tl, bottom: p_bl }
    //     right = Vertical { top: p_tr, bottom: p_br }
    let (mut reg, w, p_tl) = seeded();
    let p_tr = reg.new_terminal(w).expect("window exists");
    let p_bl = reg.new_terminal(w).expect("window exists");
    let p_br = reg.new_terminal(w).expect("window exists");
    {
        let win = reg.window_mut(w).expect("window exists");
        win.layout = Some(LayoutNode::Leaf(p_tl));
        win.split(p_tl, p_tr, SplitDir::Horizontal, 0.5)
            .expect("split tl/tr");
        win.split(p_tl, p_bl, SplitDir::Vertical, 0.5)
            .expect("split tl/bl");
        win.split(p_tr, p_br, SplitDir::Vertical, 0.5)
            .expect("split tr/br");
    }

    let win = reg.window(w).expect("window exists");

    // From the top-left:
    //   Right -> top-right
    //   Down  -> bottom-left
    //   Up    -> None (at top edge)
    //   Left  -> None (at left edge)
    assert_eq!(win.focus_direction(p_tl, Direction::Right), Some(p_tr));
    assert_eq!(win.focus_direction(p_tl, Direction::Down), Some(p_bl));
    assert_eq!(win.focus_direction(p_tl, Direction::Up), None);
    assert_eq!(win.focus_direction(p_tl, Direction::Left), None);

    // From the bottom-right:
    assert_eq!(win.focus_direction(p_br, Direction::Left), Some(p_bl));
    assert_eq!(win.focus_direction(p_br, Direction::Up), Some(p_tr));
    assert_eq!(win.focus_direction(p_br, Direction::Down), None);
    assert_eq!(win.focus_direction(p_br, Direction::Right), None);
}

// ---------------------------------------------------------------------------
// Internal consistency
// ---------------------------------------------------------------------------

#[test]
fn leaves_match_panes_after_a_sequence_of_splits() {
    let (mut reg, w, p1) = seeded();
    let p2 = reg.new_terminal(w).expect("window exists");
    let p3 = reg.new_terminal(w).expect("window exists");
    let p4 = reg.new_terminal(w).expect("window exists");
    {
        let win = reg.window_mut(w).expect("window exists");
        win.layout = Some(LayoutNode::Leaf(p1));
        win.split(p1, p2, SplitDir::Horizontal, 0.5).unwrap();
        win.split(p2, p3, SplitDir::Vertical, 0.5).unwrap();
        win.split(p1, p4, SplitDir::Vertical, 0.5).unwrap();
    }

    let win = reg.window(w).expect("window exists");
    let leaves: HashSet<ResourceId> = win.layout.as_ref().unwrap().leaves().into_iter().collect();
    let expected: HashSet<ResourceId> = [p1, p2, p3, p4].iter().copied().collect();
    assert_eq!(leaves, expected);
}
