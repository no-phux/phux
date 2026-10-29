//! Tree operations, render zoom, and CBOR encode/decode.

use super::*;

#[test]
fn checked_decode_refuses_a_known_non_terminal_leaf() {
    let ws = ws_split(1, 2, 1);
    let bytes = ws.encode_cbor().expect("encode");
    let refused = ArrangementDoc::decode_cbor_checked(&bytes, &|id| id != &t(2))
        .expect_err("a leaf naming a non-terminal resource must be refused");
    assert!(matches!(
        refused,
        LayoutDecodeError::NonTerminalLeaf(ref id) if id == &t(2)
    ));
    // Unclassifiable ids pass: the predicate only refuses what it knows.
    let accepted = ArrangementDoc::decode_cbor_checked(&bytes, &|_| true).expect("decode");
    assert_eq!(accepted, ws);
}

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
fn split_at_replaces_leaf_with_split() {
    let tree = leaf(1);
    let out = split_at(&tree, &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
    let LayoutNode::Split {
        dir,
        ratio,
        left,
        right,
    } = out
    else {
        panic!("expected Split");
    };
    assert_eq!(dir, SplitDir::Horizontal);
    assert_eq!(ratio, 0.5);
    assert!(matches!(*left, LayoutNode::Leaf(ref p) if *p == t(1)));
    assert!(matches!(*right, LayoutNode::Leaf(ref p) if *p == t(2)));
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

#[test]
fn cbor_rejects_malformed_ratio() {
    let bytes = ws_split(1, 2, 1).encode_cbor().unwrap();
    let mut forged: CborWorkspaceEnvelope = ciborium::de::from_reader(bytes.as_slice()).unwrap();
    let CborLayoutNode::Split { ratio, .. } = &mut forged.windows[0].root else {
        panic!("split")
    };
    *ratio = 2.0;
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&forged, &mut buf).unwrap();
    let err = ArrangementDoc::decode_cbor(&buf).unwrap_err();
    assert!(matches!(err, LayoutDecodeError::MalformedRatio(_)));
}

#[test]
fn unsupported_schema_versions_are_refused_without_migration() {
    for version in [1, 2, 99] {
        let value = ciborium::Value::Map(vec![(
            ciborium::Value::Text("version".into()),
            ciborium::Value::Integer(version.into()),
        )]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        assert!(
            matches!(ArrangementDoc::decode_cbor(&bytes), Err(LayoutDecodeError::UnsupportedVersion(found)) if found == version)
        );
    }
}

#[test]
fn cbor_current_focused_index_out_of_range_clamps() {
    #[derive(Serialize)]
    struct ForgedWin {
        id: [u8; 16],
        name: String,
        root: CborLayoutNode,
        focused_terminal: CborResourceId,
    }
    #[derive(Serialize)]
    struct Forged {
        version: u8,
        windows: Vec<ForgedWin>,
        focused_window_index: u32,
    }
    let forged = Forged {
        version: LAYOUT_ENVELOPE_VERSION,
        windows: vec![ForgedWin {
            id: [1; 16],
            name: "1".to_owned(),
            root: CborLayoutNode::Leaf {
                pane: CborResourceId::Local { id: 1 },
            },
            focused_terminal: CborResourceId::Local { id: 1 },
        }],
        focused_window_index: 99,
    };
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&forged, &mut buf).unwrap();
    let ws = ArrangementDoc::decode_cbor(&buf).unwrap();
    assert_eq!(ws.active, 0); // clamped to last (only) window
}

#[test]
fn cbor_arrangement_rejects_empty() {
    let err = ArrangementDoc::default().encode_cbor().unwrap_err();
    assert!(matches!(err, LayoutEncodeError::Empty));
}
