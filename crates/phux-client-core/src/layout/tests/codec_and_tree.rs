//! Tree operations, render zoom, and CBOR encode/decode.

use super::*;

#[test]
fn render_group_zooms_to_a_live_leaf() {
    let ws = ws_split(1, 2, 1);
    let rendered = ws.render_group(Some(&t(2))).expect("active window");
    // A single-leaf synthetic layout of the zoomed pane.
    assert_eq!(rendered.tree, Some(LayoutNode::Leaf(t(2))));
    assert_eq!(rendered.focus, Some(t(2)));
    // The real workspace is untouched (still a split).
    assert!(matches!(
        ws.active_group().unwrap().tree,
        Some(LayoutNode::Split { .. })
    ));
}

#[test]
fn render_group_self_heals_when_zoom_target_is_not_a_leaf() {
    // A zoom id absent from the active window (closed pane, or another
    // window's) falls back to the real layout — not a dead single pane.
    let ws = ws_split(1, 2, 1);
    let rendered = ws.render_group(Some(&t(99))).expect("active window");
    assert!(matches!(rendered.tree, Some(LayoutNode::Split { .. })));
}

#[test]
fn split_at_rejects_missing_target() {
    let tree = leaf(1);
    let err = split_at(&tree, &t(99), &t(2), SplitDir::Horizontal, 0.5).unwrap_err();
    assert!(matches!(err, LayoutError::PaneNotInLayout(_)));
}

#[test]
fn split_at_rejects_bad_ratio() {
    let tree = leaf(1);
    for bad in [0.0_f32, 1.0, -0.1, 1.1, f32::NAN] {
        let err = split_at(&tree, &t(1), &t(2), SplitDir::Horizontal, bad).unwrap_err();
        assert!(matches!(err, LayoutError::InvalidRatio(_)));
    }
}

#[test]
fn kill_pane_collapses_the_parent_and_reports_last_and_missing_leaves() {
    assert!(kill_pane(&leaf(1), &t(1)).unwrap().is_none(), "last leaf");
    assert!(matches!(
        kill_pane(&leaf(1), &t(99)),
        Err(LayoutError::PaneNotInLayout(_))
    ));
    let t1 = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
    let out = kill_pane(&t1, &t(2)).unwrap().expect("non-empty");
    assert!(matches!(out, LayoutNode::Leaf(ref p) if *p == t(1)));
    let t2 = split_at(&t1, &t(2), &t(3), SplitDir::Vertical, 0.5).unwrap();
    let out = kill_pane(&t2, &t(1)).unwrap().expect("non-empty");
    assert_eq!(leaves(&out), vec![t(2), t(3)]);
}

#[test]
fn focus_direction_right_across_split() {
    let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
    assert_eq!(focus_direction(&tree, &t(1), Direction::Right), Some(t(2)));
    assert_eq!(focus_direction(&tree, &t(2), Direction::Left), Some(t(1)));
    assert_eq!(focus_direction(&tree, &t(1), Direction::Up), None);
    assert_eq!(focus_direction(&tree, &t(99), Direction::Right), None);
}

#[test]
fn cbor_round_trip_satellite_focus() {
    let focus = ResourceId::satellite("peer.example", 42);
    let state = LayoutState {
        tree: Some(LayoutNode::Leaf(focus.clone())),
        focus: Some(focus),
    };
    let bytes = encode_state(&state);
    let decoded = ArrangementDoc::decode_cbor(&bytes).unwrap();
    assert_eq!(decoded.groups[0].state, state);
}

/// Slot tabs survive the envelope, which carries an arrangement read to the
/// TUI's layout handler: losing them there made the next write drop them.
#[test]
fn cbor_round_trip_keeps_slot_tabs() {
    let mut doc = ws_split(1, 2, 1);
    doc.slots.push(Slot {
        strip: vec![t(2), t(3), t(4)],
        default: 1,
        shown: 2,
    });
    let decoded = ArrangementDoc::decode_cbor(&doc.encode_cbor().unwrap()).unwrap();
    assert_eq!(decoded.slots, doc.slots);
}

#[test]
fn cbor_arrangement_rejects_empty() {
    let err = ArrangementDoc::default().encode_cbor().unwrap_err();
    assert!(matches!(err, LayoutEncodeError::Empty));
}
