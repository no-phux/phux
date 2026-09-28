//! Pure reflow computation for the multi-pane TUI: which leaves changed
//! SHAPE, so a `RESIZE_TERMINAL` is sent for exactly those.
//!
//! A leaf is in [`ReflowDiff::changed`] iff its `(w, h)` differs from the
//! previous snapshot or it is new; x/y movement alone is silent (a PTY cares
//! about its size, not where it is drawn). Rects come from
//! [`crate::multi_pane::pane_rects_in`], the same tiling paint uses, so a PTY
//! is always sized to exactly the rect it is painted into. The caller passes
//! the content rect left after the status bar and sidebar (ADR-0019).

use std::collections::HashMap;
use std::hash::BuildHasher;

use phux_protocol::ResourceId;

use crate::layout::{LayoutState, Rect};
use crate::multi_pane::pane_rects_in;

/// Result of [`compute_reflow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflowDiff {
    /// Leaves whose `(w, h)` differs from the previous snapshot, plus new
    /// leaves.
    pub changed: Vec<(ResourceId, Rect)>,
    /// Some leaf renders with `w < 2` or `h < 1`: the viewport is below the
    /// layout's aggregate minimums (§6.2 freezing disengaged). The caller
    /// warns and renders anyway.
    pub too_small: bool,
}

/// Tile `layout` into `content` and diff against `prev_rects`. A layout with
/// no tree yields an empty diff.
#[must_use]
pub fn compute_reflow<S: BuildHasher>(
    layout: &LayoutState,
    prev_rects: &HashMap<ResourceId, Rect, S>,
    content: Rect,
) -> ReflowDiff {
    let mut diff = ReflowDiff {
        changed: Vec::new(),
        too_small: false,
    };
    let Some(tree) = layout.tree.as_ref() else {
        return diff;
    };
    for (id, rect) in pane_rects_in(tree, content) {
        diff.too_small |= rect.w < 2 || rect.h < 1;
        match prev_rects.get(&id) {
            Some(prev) if prev.w == rect.w && prev.h == rect.h => {}
            _ => diff.changed.push((id, rect)),
        }
    }
    diff
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::layout::{LayoutNode, SplitDir, split_at};

    fn rect(cols: u16, rows: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: cols,
            h: rows,
        }
    }

    /// A tree grown by alternating splits of the newest pane, with some kills.
    fn tree_from(ops: &[Option<usize>]) -> Option<LayoutNode> {
        let mut next_id: u32 = 1;
        let mut tree = Some(LayoutNode::Leaf(ResourceId::local(next_id)));
        let mut alive = vec![ResourceId::local(next_id)];
        for op in ops {
            next_id += 1;
            let Some(cur) = tree.clone() else { break };
            match op {
                None => {
                    let pane = ResourceId::local(next_id);
                    let dir = if next_id.is_multiple_of(2) {
                        SplitDir::Horizontal
                    } else {
                        SplitDir::Vertical
                    };
                    if let Ok(t) = split_at(&cur, alive.last().unwrap(), &pane, dir, 0.5) {
                        tree = Some(t);
                        alive.push(pane);
                    }
                }
                Some(idx) => {
                    let target = alive[idx % alive.len()].clone();
                    if let Ok(t) = crate::layout::kill_pane(&cur, &target) {
                        tree = t;
                        alive.retain(|p| *p != target);
                    }
                }
            }
        }
        tree
    }

    fn arb_op() -> impl Strategy<Value = Option<usize>> {
        prop_oneof![4 => Just(None), 1 => (0_usize..16).prop_map(Some)]
    }

    #[test]
    fn a_layout_without_a_tree_yields_an_empty_diff() {
        let diff = compute_reflow(&LayoutState::default(), &HashMap::new(), rect(80, 24));
        assert!(diff.changed.is_empty() && !diff.too_small);
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

        /// A leaf is in `changed` iff it is new or its `(w, h)` changed, with
        /// its new rect; `too_small` is exactly the min-dim predicate.
        #[test]
        fn changed_is_exactly_the_reshaped_leaves(
            ops in prop::collection::vec(arb_op(), 1..15),
            cols in 0_u16..200,
            rows in 0_u16..80,
            prev_cols in 0_u16..200,
            prev_rows in 0_u16..80,
            seeded in any::<bool>(),
        ) {
            let Some(tree) = tree_from(&ops) else { return Ok(()) };
            let state = LayoutState { tree: Some(tree.clone()), focus: None };
            let prev = if seeded { pane_rects_in(&tree, rect(prev_cols, prev_rows)) } else { HashMap::new() };
            let diff = compute_reflow(&state, &prev, rect(cols, rows));
            let new_rects = pane_rects_in(&tree, rect(cols, rows));
            let changed: HashMap<_, _> = diff.changed.iter().cloned().collect();
            prop_assert_eq!(changed.len(), diff.changed.len());
            for (id, r) in &new_rects {
                let must = prev.get(id).is_none_or(|p| p.w != r.w || p.h != r.h);
                prop_assert_eq!(changed.get(id), must.then_some(r), "{:?}", id);
            }
            prop_assert!(changed.keys().all(|id| new_rects.contains_key(id)));
            prop_assert_eq!(diff.too_small, new_rects.values().any(|r| r.w < 2 || r.h < 1));
            // Identity: the same shape again changes nothing.
            prop_assert!(compute_reflow(&state, &new_rects, rect(cols, rows)).changed.is_empty());
        }
    }
}
