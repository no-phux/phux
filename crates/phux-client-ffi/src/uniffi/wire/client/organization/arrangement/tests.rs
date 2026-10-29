use phux_client_core::layout::{Group, LayoutState};

use super::*;

fn tid(id: u32) -> ResourceId {
    ResourceId::local(id)
}

fn split(dir: SplitDir, ratio: f32, left: LayoutNode, right: LayoutNode) -> LayoutNode {
    LayoutNode::Split {
        dir,
        ratio,
        left: Box::new(left),
        right: Box::new(right),
    }
}

/// One group: pane 1 left, panes 2 over 3 right; a second group: pane 4.
fn document() -> ArrangementDoc {
    let tree = split(
        SplitDir::Horizontal,
        0.5,
        LayoutNode::Leaf(tid(1)),
        split(
            SplitDir::Vertical,
            0.25,
            LayoutNode::Leaf(tid(2)),
            LayoutNode::Leaf(tid(3)),
        ),
    );
    ArrangementDoc {
        groups: vec![
            Group::with_id(
                [1; 16],
                "main".to_owned(),
                LayoutState {
                    tree: Some(tree),
                    focus: None,
                },
            ),
            Group::with_id([2; 16], "logs".to_owned(), LayoutState::single(tid(4))),
        ],
        active: 0,
    }
}

fn word(result: Result<(), WireError>) -> String {
    match result {
        Err(WireError::Runtime { reason }) => reason,
        other => panic!("not a refusal: {other:?}"),
    }
}

fn group_id(byte: u8) -> String {
    topology::group_id_text(&[byte; 16])
}

#[test]
fn groups_describe_their_trees_in_preorder() {
    let groups = describe(&document());
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].id, group_id(1));
    assert_eq!(groups[0].name, "main");
    assert_eq!(
        groups[0].nodes,
        vec![
            ArrangementNodeDescriptor::Split {
                dir: WireSplitDir::SideBySide,
                ratio: 0.5,
                first: 1,
                second: 2,
            },
            ArrangementNodeDescriptor::Leaf {
                terminal_id: "local:1".to_owned()
            },
            ArrangementNodeDescriptor::Split {
                dir: WireSplitDir::Stacked,
                ratio: 0.25,
                first: 3,
                second: 4,
            },
            ArrangementNodeDescriptor::Leaf {
                terminal_id: "local:2".to_owned()
            },
            ArrangementNodeDescriptor::Leaf {
                terminal_id: "local:3".to_owned()
            },
        ]
    );
    assert_eq!(
        groups[1].nodes,
        vec![ArrangementNodeDescriptor::Leaf {
            terminal_id: "local:4".to_owned()
        }]
    );
}

#[test]
fn set_ratio_retunes_the_split_at_a_preorder_index() {
    let mut doc = document();
    apply_edit(
        &mut doc,
        &ArrangementEdit::SetRatio {
            group_id: group_id(1),
            node: 2,
            ratio: 0.75,
        },
    )
    .unwrap();
    let nodes = &describe(&doc)[0].nodes;
    assert!(matches!(
        nodes[2],
        ArrangementNodeDescriptor::Split { ratio, .. } if ratio == 0.75
    ));
    assert!(matches!(
        nodes[0],
        ArrangementNodeDescriptor::Split { ratio, .. } if ratio == 0.5
    ));
}

#[test]
fn set_ratio_refuses_leaves_missing_nodes_groups_and_bad_ratios() {
    let edit = |group: u8, node: u32, ratio: f32| ArrangementEdit::SetRatio {
        group_id: group_id(group),
        node,
        ratio,
    };
    let mut doc = document();
    assert_eq!(word(apply_edit(&mut doc, &edit(1, 1, 0.5))), "invalid_node");
    assert_eq!(word(apply_edit(&mut doc, &edit(1, 5, 0.5))), "invalid_node");
    assert_eq!(
        word(apply_edit(&mut doc, &edit(9, 0, 0.5))),
        "invalid_group_id"
    );
    for ratio in [0.0, 1.0, -0.5, f32::NAN, f32::INFINITY] {
        assert_eq!(
            word(apply_edit(&mut doc, &edit(1, 0, ratio))),
            "invalid_ratio"
        );
    }
    assert_eq!(doc, document());
}

#[test]
fn swap_and_move_reuse_the_shared_mutations() {
    let mut doc = document();
    apply_edit(
        &mut doc,
        &ArrangementEdit::Swap {
            first: "local:1".to_owned(),
            second: "local:3".to_owned(),
        },
    )
    .unwrap();
    assert!(matches!(
        &describe(&doc)[0].nodes[1],
        ArrangementNodeDescriptor::Leaf { terminal_id } if terminal_id == "local:3"
    ));

    apply_edit(
        &mut doc,
        &ArrangementEdit::Move {
            source: "local:4".to_owned(),
            target: "local:2".to_owned(),
            dir: WireSplitDir::SideBySide,
            ratio: 0.5,
        },
    )
    .unwrap();
    // The emptied group is pruned.
    assert_eq!(doc.groups.len(), 1);
    assert!(
        phux_client_core::layout::leaves(doc.groups[0].state.tree.as_ref().unwrap())
            .contains(&tid(4))
    );
}

#[test]
fn split_places_a_new_pane_without_publishing_focus() {
    let mut doc = document();
    apply_edit(
        &mut doc,
        &ArrangementEdit::Split {
            target: "local:4".to_owned(),
            new_pane: "local:5".to_owned(),
            dir: WireSplitDir::Stacked,
            ratio: 0.5,
        },
    )
    .unwrap();
    assert_eq!(doc.active, 0);
    assert_eq!(doc.groups[1].state.focus, Some(tid(4)));
    assert_eq!(describe(&doc)[1].nodes.len(), 3);
}

#[test]
fn mutation_refusals_have_stable_words() {
    let mut doc = document();
    let cases = [
        (
            ArrangementEdit::Swap {
                first: "local:1".to_owned(),
                second: "local:1".to_owned(),
            },
            "same_pane",
        ),
        (
            ArrangementEdit::Swap {
                first: "local:1".to_owned(),
                second: "local:9".to_owned(),
            },
            "foreign_target",
        ),
        (
            ArrangementEdit::Split {
                target: "local:1".to_owned(),
                new_pane: "local:2".to_owned(),
                dir: WireSplitDir::Stacked,
                ratio: 0.5,
            },
            "duplicate_pane",
        ),
        (
            ArrangementEdit::Close {
                target: "nonsense".to_owned(),
            },
            "invalid_terminal_id",
        ),
        (
            ArrangementEdit::MoveGroup { from: 0, to: 2 },
            "invalid_group_index",
        ),
    ];
    for (edit, expected) in cases {
        assert_eq!(word(apply_edit(&mut doc, &edit)), expected, "{edit:?}");
    }
    assert_eq!(doc, document());
}

#[test]
fn closing_the_final_pane_is_refused() {
    let mut doc = ArrangementDoc::single(tid(1));
    assert_eq!(
        word(apply_edit(
            &mut doc,
            &ArrangementEdit::Close {
                target: "local:1".to_owned()
            }
        )),
        "last_pane"
    );
}

#[test]
fn move_group_reorders_tabs() {
    let mut doc = document();
    apply_edit(&mut doc, &ArrangementEdit::MoveGroup { from: 1, to: 0 }).unwrap();
    assert_eq!(describe(&doc)[0].id, group_id(2));
}

#[test]
fn disconnected_clients_hold_no_arrangement_and_refuse_edits() {
    let remote = RemoteClient::new("unused".into(), 80, 24, None, None);
    let key = SessionKey::new([2; 16]).unwrap().to_string();
    assert_eq!(remote.arrangement(key.clone()).unwrap(), None);
    assert!(remote.arrangement("bad".to_owned()).is_err());
    assert!(matches!(
        remote.edit_arrangement(key, 1, ArrangementEdit::MoveGroup { from: 0, to: 1 }),
        Err(WireError::Runtime { reason }) if reason == "not_connected"
    ));
    assert_eq!(
        remote.attach_terminal_preserving_geometry("local:1".to_owned()),
        0
    );
}
