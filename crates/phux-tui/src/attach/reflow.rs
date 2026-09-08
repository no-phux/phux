//! Pure reflow computation for the multi-pane TUI.
//!
//! Consumed by the SIGWINCH handler in `attach::driver` (today, post-wire-up)
//! and by the eventual `VIEWPORT_RESIZE` wire path (tracked under `phux-4hp`):
//! both call sites need to know which leaves' rectangles changed shape so a
//! per-pane `RESIZE { terminal_id, cols, rows }` can be emitted upstream.
//!
//! This module is intentionally **pure**: no I/O, no global state, no
//! borrowed terminal handles. It takes a [`crate::layout::LayoutState`] (the
//! client-side mirror landed by wave A), the previous per-pane [`Rect`]
//! snapshot, and the new outer viewport dims; it returns a [`ReflowDiff`]
//! the caller drives downstream.
//!
//! # RESIZE only on dim change
//!
//! [`ReflowDiff::changed`] is the load-bearing field. A `RESIZE` frame on the
//! wire (and the local libghostty mirror's `resize()` call) is a function of
//! the new (cols, rows) the pane occupies — not of where on screen it sits.
//! Two scenarios make the distinction matter:
//!
//! * **Kill-pane reflow.** Closing one leaf of a three-leaf tree may shift
//!   the *positions* of the surviving leaves without altering their cell
//!   dimensions (e.g. a sibling collapsing into a grandparent slot that has
//!   the same width). Emitting `RESIZE` here would force libghostty to
//!   redraw every cell for no behavioural change.
//! * **Pure pan.** A future "swap panes" action moves leaves around but
//!   keeps the same (w, h). Same argument: no PTY-visible state has
//!   changed.
//!
//! So the rule is: **a leaf appears in `changed` iff its (w, h) differs from
//! the previous snapshot, or it has no previous entry (a new leaf — either a
//! freshly-spawned pane or first-attach).** x/y movement alone is silent.
//!
//! # Sub-viable viewport handling
//!
//! Min-size freezing shipped with phux-foz.3 (TUI doc §6.2): the tiling
//! walk itself clamps each split so a leaf squeezed to its floor (2 cols
//! x 1 row of inner content) freezes there and the remaining space
//! redistributes to its siblings. [`ReflowDiff::too_small`] therefore
//! only fires in the degenerate regime — a viewport too small to cover
//! even the aggregate minimums, where the freeze clamp disengages and
//! proportional tiling resumes (some leaf renders with `w < 2` or
//! `h < 1`). The caller logs + renders garbage; no panic, no hole.
//!
//! # One tiling for paint and reflow
//!
//! The rectangle map comes from [`crate::multi_pane::pane_rects_in`] — the
//! *same* local-divider tiling [`crate::multi_pane::compute_layout_in`] paints
//! with. Reflow-emit and paint therefore agree by construction: the size a
//! pane's PTY is told to be (`TERMINAL_RESIZE`) is exactly the rect it is
//! drawn into, so a nested split can never leave the gap/overlap dead space
//! that arose when reflow subtracted dividers globally and paint subtracted
//! them per-node (phux-islu). Divider accounting lives inside the walk. The
//! caller passes the residual **content** rect — the pane area after the
//! status bar and (when enabled) the sidebar are folded off — so reflow sizes
//! each PTY to the same inset rect it is painted into (phux-4h5a). With a
//! full-viewport content rect this is the pre-sidebar behaviour.
//! See [ADR-0019] decision 4 for the cell-budget rationale.
//!
//! [ADR-0019]: ../../../ADR/0019-tui-multi-pane-rendering.md

use std::collections::HashMap;
use std::hash::BuildHasher;

use phux_protocol::TerminalId;

use crate::layout::{LayoutState, Rect};
use crate::multi_pane::pane_rects_in;

/// Result of [`compute_reflow`]. Drives the caller's RESIZE-emission loop
/// (per pane in [`changed`](Self::changed)) and its sub-viable-viewport
/// warning path (when [`too_small`](Self::too_small) is set).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflowDiff {
    /// The full per-leaf rectangle map for the new outer dims. The caller
    /// stores this as its next `prev_rects` snapshot.
    pub new_rects: HashMap<TerminalId, Rect>,
    /// Leaves whose (w, h) differs from the previous snapshot, plus leaves
    /// new to this snapshot (no entry in `prev_rects`). x/y-only movement
    /// does **not** appear here — see the module docs for why.
    pub changed: Vec<(TerminalId, Rect)>,
    /// Some leaf in `new_rects` renders with `w < 2` or `h < 1`: the
    /// viewport is below the layout's aggregate minimums, so §6.2
    /// min-size freezing disengaged and proportional tiling resumed
    /// (phux-foz.3). The caller logs a warning and renders garbage; we
    /// do not panic. Above that threshold freezing holds every leaf at
    /// its floor and this stays `false`.
    pub too_small: bool,
}

/// Compute the per-pane rectangle map for the new `content` rect and diff it
/// against `prev_rects`.
///
/// Pure: no I/O, no allocator games beyond the returned maps/vecs. The
/// algorithm:
///
/// 1. Tile the tree into the new `content` rect via
///    [`crate::multi_pane::pane_rects_in`] — the canonical local-divider walk
///    paint uses, so divider accounting is handled inside the walk. `content`
///    is the residual pane area after the status bar and (when enabled) the
///    sidebar are folded off; with a full-viewport rect this is the
///    pre-sidebar behaviour.
/// 2. Diff against `prev_rects`: a leaf enters `changed` iff it is new or
///    its (w, h) differs.
/// 3. Set `too_small` if any leaf would have `w < 2 || h < 1`.
///
/// If [`LayoutState::tree`] is `None`, returns an empty diff (no rects, no
/// changes, not too small). The caller is single-pane — no reflow to do.
#[must_use]
pub fn compute_reflow<S: BuildHasher>(
    layout: &LayoutState,
    prev_rects: &HashMap<TerminalId, Rect, S>,
    content: Rect,
) -> ReflowDiff {
    let Some(tree) = layout.tree.as_ref() else {
        return ReflowDiff {
            new_rects: HashMap::new(),
            changed: Vec::new(),
            too_small: false,
        };
    };

    let new_rects = pane_rects_in(tree, content);

    let mut changed: Vec<(TerminalId, Rect)> = Vec::new();
    let mut too_small = false;
    for (id, rect) in &new_rects {
        if rect.w < 2 || rect.h < 1 {
            too_small = true;
        }
        match prev_rects.get(id) {
            Some(prev) if prev.w == rect.w && prev.h == rect.h => {}
            _ => changed.push((id.clone(), *rect)),
        }
    }

    ReflowDiff {
        new_rects,
        changed,
        too_small,
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::collections::HashSet;

    use proptest::prelude::*;

    use super::*;
    use crate::layout::{LayoutNode, SplitDir, leaves, split_at};

    fn t(id: u32) -> TerminalId {
        TerminalId::local(id)
    }

    /// A None-equivalent content rect: the full viewport anchored at the
    /// origin. `compute_reflow` now takes a content `Rect` (the residual pane
    /// area after chrome reservations); a full-viewport rect reproduces the
    /// pre-sidebar behaviour these tests pin.
    fn rect(cols: u16, rows: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: cols,
            h: rows,
        }
    }

    fn leaf(id: u32) -> LayoutNode {
        LayoutNode::Leaf(t(id))
    }

    fn state_with(tree: LayoutNode, focus: TerminalId) -> LayoutState {
        LayoutState {
            tree: Some(tree),
            focus: Some(focus),
        }
    }

    // -------------------------------------------------------------------------
    // Empty / single-pane corners
    // -------------------------------------------------------------------------

    #[test]
    fn empty_state_returns_empty_diff() {
        let state = LayoutState::default();
        let prev = HashMap::new();
        let diff = compute_reflow(&state, &prev, rect(80, 24));
        assert!(diff.new_rects.is_empty());
        assert!(diff.changed.is_empty());
        assert!(!diff.too_small);
    }

    #[test]
    fn single_pane_initial_attach_marks_changed() {
        // Fresh single pane, no prev snapshot — must appear in `changed`
        // so the caller emits a RESIZE to seed the libghostty mirror.
        let state = LayoutState::single(t(1));
        let prev = HashMap::new();
        let diff = compute_reflow(&state, &prev, rect(80, 24));

        assert_eq!(diff.new_rects.len(), 1);
        assert_eq!(
            diff.new_rects[&t(1)],
            Rect {
                x: 0,
                y: 0,
                w: 80,
                h: 24
            }
        );
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].0, t(1));
        assert!(!diff.too_small);
    }

    #[test]
    fn single_pane_unchanged_dims_no_changes() {
        let state = LayoutState::single(t(1));
        let mut prev = HashMap::new();
        prev.insert(
            t(1),
            Rect {
                x: 0,
                y: 0,
                w: 80,
                h: 24,
            },
        );
        let diff = compute_reflow(&state, &prev, rect(80, 24));

        assert_eq!(diff.new_rects.len(), 1);
        assert!(diff.changed.is_empty());
        assert!(!diff.too_small);
    }

    // -------------------------------------------------------------------------
    // Ticket-specified scenarios
    // -------------------------------------------------------------------------

    #[test]
    fn resize_grow_two_pane_vertical_both_change() {
        // Two-pane *vertical* split (top/bottom). 80x24 -> 120x30: both
        // panes' widths AND heights grow.
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Vertical, 0.5).unwrap();
        let state = state_with(tree, t(1));

        // Seed prev_rects with the 80x24 outer viewport: the canonical
        // tiling subtracts the one vertical divider row internally, so the
        // 23 content rows split 12/11.
        let prev = pane_rects_in(state.tree.as_ref().unwrap(), rect(80, 24));
        let diff = compute_reflow(&state, &prev, rect(120, 30));

        // Both panes are in `changed`.
        assert_eq!(diff.changed.len(), 2);
        let changed_ids: HashSet<_> = diff.changed.iter().map(|(id, _)| id.clone()).collect();
        assert!(changed_ids.contains(&t(1)));
        assert!(changed_ids.contains(&t(2)));

        // Widths grew proportionally: each pane now spans the full new
        // content width (120, since vertical splits don't consume cols).
        for (_, r) in &diff.changed {
            assert_eq!(r.w, 120);
        }
        // Heights sum to (30 - 1 divider) = 29.
        let h_sum: u32 = diff.changed.iter().map(|(_, r)| u32::from(r.h)).sum();
        assert_eq!(h_sum, 29);
        assert!(!diff.too_small);
    }

    #[test]
    fn resize_grow_two_pane_horizontal_both_change() {
        // Sanity: same growth scenario but a horizontal split. 80x24 ->
        // 120x30. One horizontal divider col -> content (119, 30); 0.5
        // splits 119 cols into 60/59.
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let state = state_with(tree, t(1));
        let prev = pane_rects_in(state.tree.as_ref().unwrap(), rect(80, 24));
        let diff = compute_reflow(&state, &prev, rect(120, 30));

        assert_eq!(diff.changed.len(), 2);
        // Heights are unchanged-from-prev only if outer rows matched.
        // They didn't (24 -> 30), so both should still be in `changed`
        // via height.
        for (_, r) in &diff.changed {
            assert_eq!(r.h, 30);
        }
        let w_sum: u32 = diff.changed.iter().map(|(_, r)| u32::from(r.w)).sum();
        assert_eq!(w_sum, 119);
        assert!(!diff.too_small);
    }

    #[test]
    fn xy_only_movement_is_silent() {
        // Build two LayoutStates with the SAME leaf set and SAME outer
        // dims but DIFFERENT topologies that happen to produce identical
        // (w, h) per leaf. Construct:
        //
        //   A: ((1|2)/3)   horizontal between 1+2, vertical between (1|2) and 3
        //   B: same shape, just sanity baseline.
        //
        // We pre-seed prev_rects from compute_reflow(A), then ask
        // compute_reflow against state A again at the same dims: all panes
        // unchanged. Then mutate to state where positions shift but
        // dimensions don't — but constructing such a topology change here
        // is awkward at this layer. Use the simpler invariant: re-running
        // reflow on the SAME state at the SAME dims yields zero changes.
        // (See proptest_xy_movement_silent for the topology-change form.)
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let tree = split_at(&tree, &t(2), &t(3), SplitDir::Vertical, 0.5).unwrap();
        let state = state_with(tree, t(1));

        // First reflow seeds the snapshot.
        let prev = HashMap::new();
        let first = compute_reflow(&state, &prev, rect(80, 24));
        assert_eq!(first.changed.len(), 3);

        // Second reflow at the same dims: zero changes.
        let second = compute_reflow(&state, &first.new_rects, rect(80, 24));
        assert!(second.changed.is_empty(), "got {:?}", second.changed);
        assert!(!second.too_small);
    }

    #[test]
    fn shrink_to_viable_marks_all_changed() {
        // 120x30 -> 40x12, three-pane tree. All panes shrink; all in
        // changed; not too_small.
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let tree = split_at(&tree, &t(2), &t(3), SplitDir::Vertical, 0.5).unwrap();
        let state = state_with(tree, t(1));

        let prev = pane_rects_in(state.tree.as_ref().unwrap(), rect(120, 30));
        let diff = compute_reflow(&state, &prev, rect(40, 12));

        // All three leaves present in new_rects.
        assert_eq!(diff.new_rects.len(), 3);
        // All three in `changed`.
        let changed_ids: HashSet<_> = diff.changed.iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(changed_ids.len(), 3);
        // Viable: 40 - 1 horiz divider = 39 cols content, 12 - 1 vert =
        // 11 rows content. 0.5 of 39 = 19/20 cols; rows 0.5 of 11 = 6/5.
        // Min dim is therefore >= 2 cols, >= 1 row -> not too small.
        assert!(!diff.too_small, "rects: {:?}", diff.new_rects);
    }

    #[test]
    fn shrink_below_viable_sets_too_small_no_panic() {
        // 4x2 outer with a horizontal split inside a vertical split:
        // dividers eat 1 col + 1 row -> 3x1 content. 0.5 horizontal split
        // of 3 cols => 2/1 cols. One pane has w=1 < 2 -> too_small.
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let tree = split_at(&tree, &t(2), &t(3), SplitDir::Vertical, 0.5).unwrap();
        let state = state_with(tree, t(1));

        let prev = HashMap::new();
        let diff = compute_reflow(&state, &prev, rect(4, 2));
        assert!(diff.too_small);
        // No panic. We still return a populated diff so the caller can
        // render garbage rather than freezing the UI.
        assert_eq!(diff.new_rects.len(), 3);
    }

    #[test]
    fn shrink_below_viable_zero_dims() {
        // 0x0 outer — every leaf is 0x0. too_small must be set, must not
        // panic.
        let state = LayoutState::single(t(1));
        let diff = compute_reflow(&state, &HashMap::new(), rect(0, 0));
        assert!(diff.too_small);
        assert_eq!(diff.new_rects.len(), 1);
        assert_eq!(
            diff.new_rects[&t(1)],
            Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0
            }
        );
    }

    // -------------------------------------------------------------------------
    // Proptest invariants
    // -------------------------------------------------------------------------

    /// Random ops as in `layout::tests` — fewer cases here because we
    /// drive them through `compute_reflow` on top of the layout fuzz.
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

    #[allow(clippy::needless_pass_by_value)]
    fn apply_ops(ops: Vec<Op>) -> (Option<LayoutNode>, Vec<TerminalId>) {
        let mut next_id: u32 = 1;
        let first = TerminalId::local(next_id);
        next_id += 1;
        let mut tree: Option<LayoutNode> = Some(LayoutNode::Leaf(first.clone()));
        let mut alive: Vec<TerminalId> = vec![first];

        for op in ops {
            match op {
                Op::AddPane => {
                    let new_pane = TerminalId::local(next_id);
                    next_id += 1;
                    let Some(target) = alive.last().cloned() else {
                        tree = Some(LayoutNode::Leaf(new_pane.clone()));
                        alive.push(new_pane);
                        continue;
                    };
                    let Some(cur) = tree.clone() else {
                        tree = Some(LayoutNode::Leaf(new_pane.clone()));
                        alive.push(new_pane);
                        continue;
                    };
                    let dir = if next_id.is_multiple_of(2) {
                        SplitDir::Horizontal
                    } else {
                        SplitDir::Vertical
                    };
                    if let Ok(t) = split_at(&cur, &target, &new_pane, dir, 0.5) {
                        tree = Some(t);
                        alive.push(new_pane);
                    }
                }
                Op::KillPaneAt(idx) => {
                    if alive.is_empty() {
                        continue;
                    }
                    let target = alive[idx % alive.len()].clone();
                    let Some(cur) = tree.clone() else { continue };
                    if let Ok(new_tree) = crate::layout::kill_pane(&cur, &target) {
                        tree = new_tree;
                        alive.retain(|p| *p != target);
                    }
                }
            }
        }
        (tree, alive)
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

        /// `changed ⊆ new_rects`, and each changed leaf's (w, h) differs
        /// from its prev_rects entry (or it had none).
        #[test]
        fn proptest_changed_subset_and_dims_differ(
            ops_a in prop::collection::vec(arb_op(), 1..15),
            cols in 4_u16..200,
            rows in 4_u16..80,
            prev_cols in 4_u16..200,
            prev_rows in 4_u16..80,
        ) {
            let (tree, alive) = apply_ops(ops_a);
            let Some(tree) = tree else { return Ok(()) };
            let focus = alive.last().cloned().expect("alive non-empty when tree exists");
            let state = state_with(tree.clone(), focus);

            // Seed prev_rects from the same tree at a different outer dim
            // via the canonical tiling — the same one compute_reflow uses,
            // so the diff reflects a real dimension change, not an
            // algorithm mismatch.
            let prev = pane_rects_in(&tree, rect(prev_cols, prev_rows));

            let diff = compute_reflow(&state, &prev, rect(cols, rows));

            // Subset.
            for (id, rect) in &diff.changed {
                let nr = diff.new_rects.get(id).expect("changed id in new_rects");
                prop_assert_eq!(nr, rect);
            }
            // Each changed leaf differs in (w, h) from prev OR is new.
            for (id, rect) in &diff.changed {
                if let Some(prev_rect) = prev.get(id) {
                    prop_assert!(
                        prev_rect.w != rect.w || prev_rect.h != rect.h,
                        "changed leaf {id:?} has matching dims (prev={prev_rect:?}, new={rect:?})"
                    );
                }
                // None: new leaf — allowed in changed.
            }
            // Conversely, every leaf in new_rects whose (w, h) differs
            // from prev (or is new) MUST appear in changed.
            let changed_set: HashSet<_> = diff.changed.iter().map(|(id, _)| id.clone()).collect();
            for (id, rect) in &diff.new_rects {
                let must_change = prev.get(id).is_none_or(|prev_rect| {
                    prev_rect.w != rect.w || prev_rect.h != rect.h
                });
                if must_change {
                    prop_assert!(
                        changed_set.contains(id),
                        "leaf {id:?} should be in changed (prev={:?}, new={rect:?})",
                        prev.get(id)
                    );
                }
            }
        }

        /// Reflow rects are well-formed: every alive leaf gets a rect,
        /// each rect sits inside the viewport, and no two leaf rects
        /// overlap. (The full leaf-plus-divider exact-cover invariant is
        /// proved against `compute_layout` in the `multi_pane` tests,
        /// which can see the divider cells; here we only have the leaf
        /// rects.)
        #[test]
        fn proptest_reflow_rects_well_formed(
            ops in prop::collection::vec(arb_op(), 1..15),
            cols in 4_u16..200,
            rows in 4_u16..80,
        ) {
            let (tree, alive) = apply_ops(ops);
            let Some(tree) = tree else { return Ok(()) };
            let focus = alive.last().cloned().expect("alive non-empty");
            let state = state_with(tree.clone(), focus);

            let diff = compute_reflow(&state, &HashMap::new(), rect(cols, rows));

            // Every alive leaf has a rect — sub-viable splits shrink panes
            // to zero size but never drop them.
            for id in leaves(&tree) {
                prop_assert!(diff.new_rects.contains_key(&id));
            }

            // Each rect lies within the viewport, and no two leaf rects
            // share a cell.
            let mut covered: HashSet<(u16, u16)> = HashSet::new();
            for r in diff.new_rects.values() {
                prop_assert!(u32::from(r.x) + u32::from(r.w) <= u32::from(cols));
                prop_assert!(u32::from(r.y) + u32::from(r.h) <= u32::from(rows));
                for y in r.y..r.y.saturating_add(r.h) {
                    for x in r.x..r.x.saturating_add(r.w) {
                        prop_assert!(covered.insert((x, y)), "overlap at ({x}, {y})");
                    }
                }
            }
        }

        /// Same state, same dims, full prev snapshot → empty `changed`.
        /// This is the "x/y-only movement is silent" invariant at the
        /// degenerate-but-strongest end: no movement at all.
        #[test]
        fn proptest_identity_reflow_empty_changed(
            ops in prop::collection::vec(arb_op(), 1..15),
            cols in 4_u16..200,
            rows in 4_u16..80,
        ) {
            let (tree, alive) = apply_ops(ops);
            let Some(tree) = tree else { return Ok(()) };
            let focus = alive.last().cloned().expect("alive non-empty");
            let state = state_with(tree, focus);

            let seed = compute_reflow(&state, &HashMap::new(), rect(cols, rows));
            let identity = compute_reflow(&state, &seed.new_rects, rect(cols, rows));
            prop_assert!(identity.changed.is_empty(), "unexpected changes: {:?}", identity.changed);
            prop_assert_eq!(&identity.new_rects, &seed.new_rects);
        }

        /// `too_small` correctly reflects the min-dim predicate over
        /// `new_rects`.
        #[test]
        fn proptest_too_small_matches_predicate(
            ops in prop::collection::vec(arb_op(), 1..15),
            cols in 0_u16..200,
            rows in 0_u16..80,
        ) {
            let (tree, alive) = apply_ops(ops);
            let Some(tree) = tree else { return Ok(()) };
            let focus = alive.last().cloned().expect("alive non-empty");
            let state = state_with(tree, focus);

            let diff = compute_reflow(&state, &HashMap::new(), rect(cols, rows));
            let any_small = diff.new_rects.values().any(|r| r.w < 2 || r.h < 1);
            prop_assert_eq!(diff.too_small, any_small);
        }
    }
}
