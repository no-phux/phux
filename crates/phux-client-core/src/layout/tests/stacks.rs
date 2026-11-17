//! Stacks: several panes in one slot, one shown.
use super::*;
use crate::layout::wire::{HandleMap, from_protocol, to_protocol};
use phux_protocol::ids::ResourceKey;
use phux_protocol::wire::frame::{
    ArrangementDoc as WireDoc, ArrangementGroup as WireGroup, ArrangementNode as WireNode,
    EntryRef, EntryTarget,
};

/// `1 | (2 over 3)`, focus on 1.
fn three() -> ArrangementDoc {
    let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
    let tree = split_at(&tree, &t(2), &t(3), SplitDir::Vertical, 0.5).unwrap();
    ArrangementDoc {
        groups: vec![Group::new(
            "1".to_owned(),
            LayoutState {
                tree: Some(tree),
                focus: Some(t(1)),
            },
        )],
        active: 0,
    }
}

fn tree(doc: &ArrangementDoc) -> &LayoutNode {
    doc.groups[0].state.tree.as_ref().unwrap()
}

fn right(doc: &ArrangementDoc) -> &LayoutNode {
    let LayoutNode::Split { right, .. } = tree(doc) else {
        panic!("split");
    };
    right
}

#[test]
fn stacking_takes_the_pane_out_of_its_slot_and_shows_it_on_the_other() {
    let mut doc = three();
    apply_mutation(
        &mut doc,
        &LayoutMutation::Stack {
            source: t(3),
            target: t(2),
        },
    )
    .unwrap();
    assert_eq!(
        right(&doc),
        &LayoutNode::Stack {
            active: 1,
            panes: vec![t(2), t(3)],
        }
    );
    assert_eq!(doc.groups[0].state.focus, Some(t(3)));
    // Every pane is still arranged; only the shown ones are tiled.
    assert_eq!(leaves(tree(&doc)), vec![t(1), t(2), t(3)]);
    assert_eq!(shown_leaves(tree(&doc)), vec![t(1), t(3)]);
    let rects = crate::multi_pane::pane_rects(tree(&doc), (81, 24));
    assert_eq!(rects.len(), 2);
    assert_eq!(rects[&t(3)].w, 40);
    assert_eq!(rects[&t(3)].h, 24);
}

#[test]
fn showing_and_unstacking_keep_the_rest_of_the_layout() {
    let mut doc = three();
    apply_mutation(
        &mut doc,
        &LayoutMutation::Stack {
            source: t(3),
            target: t(2),
        },
    )
    .unwrap();
    apply_mutation(&mut doc, &LayoutMutation::Reveal { pane: t(2) }).unwrap();
    assert_eq!(
        stack_of(tree(&doc), &t(3)),
        Some((0, [t(2), t(3)].as_slice()))
    );
    assert_eq!(doc.groups[0].state.focus, Some(t(2)));
    // Directional focus lands on what the stack shows.
    assert_eq!(
        focus_direction(tree(&doc), &t(1), Direction::Right),
        Some(t(2))
    );

    apply_mutation(
        &mut doc,
        &LayoutMutation::Unstack {
            pane: t(2),
            dir: SplitDir::Vertical,
        },
    )
    .unwrap();
    assert_eq!(
        right(&doc),
        &LayoutNode::Split {
            dir: SplitDir::Vertical,
            ratio: 0.5,
            left: Box::new(leaf(3)),
            right: Box::new(leaf(2)),
        }
    );
    assert_eq!(
        apply_mutation(&mut doc, &LayoutMutation::Reveal { pane: t(1) }),
        Err(MutationError::NotStacked(t(1)))
    );
}

#[test]
fn closing_a_stacked_pane_shrinks_the_stack_to_a_leaf() {
    let mut doc = three();
    apply_mutation(
        &mut doc,
        &LayoutMutation::Stack {
            source: t(3),
            target: t(2),
        },
    )
    .unwrap();
    apply_mutation(&mut doc, &LayoutMutation::Close { target: t(3) }).unwrap();
    assert_eq!(right(&doc), &leaf(2));
}

fn key(n: u8) -> ResourceKey {
    ResourceKey::new([n; 16]).unwrap()
}

fn entry(n: u8) -> EntryRef {
    EntryRef {
        entry: [0xe0 | n; 16],
        target: EntryTarget::Member(key(n)),
    }
}

fn stored(entries: Vec<EntryRef>) -> WireDoc {
    WireDoc {
        groups: vec![WireGroup::new(
            [1; 16],
            Some("one".to_owned()),
            WireNode::Stack { active: 1, entries },
        )],
        stacks: true,
    }
}

#[test]
fn a_stack_round_trips_and_one_it_cannot_show_rejoins_its_stack() {
    let both = HandleMap::from_pairs([(key(1), Some(t(11))), (key(2), Some(t(12)))]);
    let doc = stored(vec![entry(1), entry(2)]);
    let (client, binding) = from_protocol(&doc, &both);
    assert_eq!(
        tree(&client),
        &LayoutNode::Stack {
            active: 1,
            panes: vec![t(11), t(12)],
        }
    );
    assert_eq!(to_protocol(&client, &both, &binding).unwrap(), doc);

    // Entry 2 isn't running here: the client sees entry 1 alone, and writes
    // entry 2 back into its stack rather than as a new split.
    let one = HandleMap::from_pairs([(key(1), Some(t(11))), (key(2), None)]);
    let (client, binding) = from_protocol(&doc, &one);
    assert_eq!(tree(&client), &leaf(11));
    let written = to_protocol(&client, &one, &binding).unwrap();
    assert_eq!(
        written.groups[0].tree,
        WireNode::Stack {
            active: 0,
            entries: vec![entry(1), entry(2)],
        }
    );
}
