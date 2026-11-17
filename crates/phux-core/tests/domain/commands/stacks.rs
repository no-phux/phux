//! Stacks: several entries in one slot, one shown.
use super::*;

fn stacked(r1: ResourceKey, r2: ResourceKey, r3: ResourceKey) -> Vec<Group> {
    vec![Group {
        locked: false,
        id: [5; 16],
        name: None,
        tree: LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutNode::Leaf(EntryRef::member([1; 16], r1))),
            right: Box::new(LayoutNode::Stack {
                active: 1,
                entries: vec![EntryRef::member([2; 16], r2), EntryRef::member([3; 16], r3)],
            }),
        },
    }]
}

fn three(h: &mut Harness) -> (SessionKey, [ResourceKey; 3]) {
    let a = h.create_session("a", SessionLifetime::Persistent);
    let r = [h.shell_in(a), h.shell_in(a), h.shell_in(a)];
    h.run(Op::ArrangementReplace {
        owner: ArrangementOwner::Session(a),
        expected: Revision::FIRST,
        groups: stacked(r[0], r[1], r[2]),
        stacks: true,
    })
    .expect("arrangement");
    (a, r)
}

fn tree(h: &Harness, a: SessionKey) -> LayoutNode<EntryRef> {
    let key = h.session(a).arrangement.expect("linked");
    h.reg.org().arrangement(&key).expect("kept").groups[0]
        .tree
        .clone()
}

#[test]
pub(super) fn a_hidden_entry_counts_as_arranged_and_leaves_with_its_resource() {
    let mut h = Harness::new();
    let (a, [r1, r2, r3]) = three(&mut h);
    let b = h.create_session("b", SessionLifetime::Persistent);
    let key = h.session(a).arrangement.expect("linked");
    assert_eq!(
        h.reg.org().arrangement(&key).expect("kept").resources(),
        vec![r1, r2, r3]
    );
    let (ra, rb) = (h.session(a).revision, h.session(b).revision);
    h.run(Op::ResourceMove {
        resource: r2,
        from: a,
        from_expected: ra,
        to: b,
        to_expected: rb,
    })
    .expect("move");
    // The stack of two loses one and is a plain leaf again, still r3.
    assert_eq!(
        tree(&h, a),
        LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutNode::Leaf(EntryRef::member([1; 16], r1))),
            right: Box::new(LayoutNode::Leaf(EntryRef::member([3; 16], r3))),
        }
    );
}

#[test]
pub(super) fn a_writer_that_does_not_know_stacks_keeps_them() {
    let mut h = Harness::new();
    let (a, [r1, r2, r3]) = three(&mut h);
    // It saw the stack as its shown entry, r3, and swapped the two halves.
    let seen = vec![Group {
        locked: false,
        id: [5; 16],
        name: None,
        tree: LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutNode::Leaf(EntryRef::member([3; 16], r3))),
            right: Box::new(LayoutNode::Leaf(EntryRef::member([1; 16], r1))),
        },
    }];
    h.run(Op::ArrangementReplace {
        owner: ArrangementOwner::Session(a),
        expected: Revision(2),
        groups: seen,
        stacks: false,
    })
    .expect("replace");
    let LayoutNode::Split { left, .. } = tree(&h, a) else {
        panic!("still split");
    };
    assert_eq!(
        *left,
        LayoutNode::Stack {
            active: 1,
            entries: vec![EntryRef::member([2; 16], r2), EntryRef::member([3; 16], r3)],
        }
    );
}

#[test]
pub(super) fn a_hidden_entry_an_older_writer_placed_stays_where_it_put_it() {
    let mut h = Harness::new();
    let (a, [r1, r2, r3]) = three(&mut h);
    // `reopen-views` gave the hidden r2 a tab of its own.
    let mut seen = stacked(r1, r2, r3);
    seen[0].tree = LayoutNode::Split {
        dir: SplitDir::Horizontal,
        ratio: 0.5,
        left: Box::new(LayoutNode::Leaf(EntryRef::member([1; 16], r1))),
        right: Box::new(LayoutNode::Leaf(EntryRef::member([3; 16], r3))),
    };
    seen.push(Group {
        locked: false,
        id: [6; 16],
        name: None,
        tree: LayoutNode::Leaf(EntryRef::member([2; 16], r2)),
    });
    h.run(Op::ArrangementReplace {
        owner: ArrangementOwner::Session(a),
        expected: Revision(2),
        groups: seen.clone(),
        stacks: false,
    })
    .expect("replace");
    let key = h.session(a).arrangement.expect("linked");
    let groups = &h.reg.org().arrangement(&key).expect("kept").groups;
    assert_eq!(groups[0].tree, seen[0].tree);
    assert_eq!(groups[1].tree, seen[1].tree);
}

#[test]
pub(super) fn a_stack_needs_two_entries_and_a_shown_one() {
    let mut h = Harness::new();
    let (a, [r1, r2, _]) = three(&mut h);
    for (active, entries) in [
        (0, vec![EntryRef::member([2; 16], r2)]),
        (
            2,
            vec![EntryRef::member([2; 16], r2), EntryRef::member([4; 16], r1)],
        ),
    ] {
        let groups = vec![Group {
            locked: false,
            id: [5; 16],
            name: None,
            tree: LayoutNode::Stack { active, entries },
        }];
        assert!(matches!(
            h.run(Op::ArrangementReplace {
                owner: ArrangementOwner::Session(a),
                expected: Revision(2),
                groups,
                stacks: true,
            }),
            Err(Refusal::InvalidArgument {
                field: "groups",
                ..
            })
        ));
    }
}

#[test]
pub(super) fn collapsing_a_stack_keeps_the_shown_entry_when_it_survives() {
    use phux_core::arrangement::collapse;
    let node = LayoutNode::Stack {
        active: 2,
        entries: vec![1, 2, 3, 4],
    };
    assert_eq!(
        collapse(node.clone(), &|e| *e == 1),
        Some(LayoutNode::Stack {
            active: 1,
            entries: vec![2, 3, 4],
        })
    );
    // The shown one gone: the one after it shows.
    assert_eq!(
        collapse(node.clone(), &|e| *e == 3),
        Some(LayoutNode::Stack {
            active: 2,
            entries: vec![1, 2, 4],
        })
    );
    assert_eq!(
        collapse(node.clone(), &|e| *e != 4),
        Some(LayoutNode::Leaf(4))
    );
    assert_eq!(collapse(node, &|_| true), None);
}
