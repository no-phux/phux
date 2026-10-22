//! Property tests over random split/kill/focus sequences.

use super::*;

#[derive(Debug, Clone, Copy)]
enum Op {
    AddPane,
    KillPaneAt(usize),
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => Just(Op::AddPane),
        1 => (0_usize..16).prop_map(Op::KillPaneAt),
    ]
}

/// Apply `ops` against a fresh single-leaf tree, returning the final
/// tree (or `None` if killed empty) plus the ordered list of leaves
/// that should currently live in the tree.
#[allow(clippy::needless_pass_by_value)]
fn apply_ops(ops: Vec<Op>) -> (Option<LayoutNode>, Vec<ResourceId>) {
    apply_ops_from(ops, 1)
}

fn apply_ops_from(ops: Vec<Op>, mut next_id: u32) -> (Option<LayoutNode>, Vec<ResourceId>) {
    let first = ResourceId::local(next_id);
    next_id += 1;
    let mut tree: Option<LayoutNode> = Some(LayoutNode::Leaf(first.clone()));
    let mut alive: Vec<ResourceId> = vec![first];

    for op in ops {
        match op {
            Op::AddPane => {
                let new_pane = ResourceId::local(next_id);
                next_id += 1;
                let Some(target) = alive.last().cloned() else {
                    // Tree was empty — reseed.
                    tree = Some(LayoutNode::Leaf(new_pane.clone()));
                    alive.push(new_pane);
                    continue;
                };
                let Some(cur) = tree else {
                    tree = Some(LayoutNode::Leaf(new_pane.clone()));
                    alive.push(new_pane);
                    continue;
                };
                let dir = if next_id.is_multiple_of(2) {
                    SplitDir::Horizontal
                } else {
                    SplitDir::Vertical
                };
                match split_at(&cur, &target, &new_pane, dir, 0.5) {
                    Ok(t) => {
                        tree = Some(t);
                        alive.push(new_pane);
                    }
                    Err(_) => {
                        // Restore tree and skip.
                        tree = Some(cur);
                    }
                }
            }
            Op::KillPaneAt(idx) => {
                if alive.is_empty() {
                    continue;
                }
                let target = alive[idx % alive.len()].clone();
                let Some(cur) = tree else { continue };
                match kill_pane(&cur, &target) {
                    Ok(new_tree) => {
                        tree = new_tree;
                        alive.retain(|p| *p != target);
                    }
                    Err(_) => {
                        tree = Some(cur);
                    }
                }
            }
        }
    }
    (tree, alive)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Invariant 1: every ResourceId appears as exactly one leaf.
    #[test]
    fn proptest_leaves_match_alive(ops in prop::collection::vec(arb_op(), 1..20)) {
        let (tree, alive) = apply_ops(ops);
        let tree_leaves = tree.as_ref().map_or_else(Vec::new, leaves);
        let alive_set: HashSet<_> = alive.into_iter().collect();
        let leaf_set: HashSet<_> = tree_leaves.iter().cloned().collect();
        // Set equality.
        prop_assert_eq!(&alive_set, &leaf_set);
        // Exactly one leaf per id (no duplicates).
        prop_assert_eq!(tree_leaves.len(), leaf_set.len());
    }

    /// Invariant 3: `focus_direction` is partial, deterministic, and
    /// only returns ids that are leaves of the tree.
    #[test]
    fn proptest_focus_direction_partial_deterministic(
        ops in prop::collection::vec(arb_op(), 1..20),
        dir_pick in 0_u8..4,
    ) {
        let (tree, alive) = apply_ops(ops);
        let Some(tree) = tree else { return Ok(()) };
        if alive.is_empty() { return Ok(()) }
        let leaf_set: HashSet<_> = leaves(&tree).into_iter().collect();
        let dir = match dir_pick {
            0 => Direction::Up,
            1 => Direction::Down,
            2 => Direction::Left,
            _ => Direction::Right,
        };
        for src in &alive {
            let a = focus_direction(&tree, src, dir);
            let b = focus_direction(&tree, src, dir);
            // Deterministic.
            prop_assert_eq!(&a, &b);
            if let Some(neighbour) = a {
                // Neighbour is a leaf of the tree.
                prop_assert!(leaf_set.contains(&neighbour));
                // Different from source.
                prop_assert_ne!(&neighbour, src);
            }
        }
    }

    /// Invariant 4: CBOR round-trips for any state derived from
    /// random ops (with focus on the last surviving leaf).
    #[test]
    fn proptest_cbor_round_trip(ops in prop::collection::vec(arb_op(), 1..15)) {
        let (tree, alive) = apply_ops(ops);
        let Some(tree) = tree else { return Ok(()) };
        let Some(focus) = alive.last().cloned() else { return Ok(()) };
        let state = LayoutState { tree: Some(tree), focus: Some(focus) };
        let bytes = encode_state(&state);
        let decoded = ArrangementDoc::decode_cbor(&bytes).expect("decode");
        prop_assert_eq!(&decoded.groups[0].state, &state);
    }

    /// Invariant 5: a multi-window [`ArrangementDoc`] CBOR-round-trips for
    /// any groups derived from random ops.
    #[test]
    fn proptest_arrangement_cbor_round_trip(
        per_window in prop::collection::vec(
            prop::collection::vec(arb_op(), 1..10), 1..5),
    ) {
        let mut groups = Vec::new();
        for (i, ops) in per_window.into_iter().enumerate() {
            // A workspace has one durable namespace, not independently
            // reused terminal IDs in each generated window.
            let seed = u32::try_from(i).unwrap() * 100 + 1;
            let (tree, alive) = apply_ops_from(ops, seed);
            let (Some(tree), Some(focus)) = (tree, alive.last().cloned()) else {
                continue;
            };
            groups.push(Group::new((i + 1).to_string(), LayoutState { tree: Some(tree), focus: Some(focus) }));
        }
        prop_assume!(!groups.is_empty());
        let active = groups.len() / 2;
        let ws = ArrangementDoc { groups, active };
        let bytes = ws.encode_cbor().expect("encode");
        let decoded = ArrangementDoc::decode_cbor(&bytes).expect("decode");
        prop_assert_eq!(decoded, ws);
    }
}
