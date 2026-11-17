//! Multi-pane composition: per-pane rectangles and the divider cells between.
//!
//! Dividers use the light box-drawing set (ADR-0019). Each `Horizontal`
//! split consumes one column and each `Vertical` split one row; focus
//! emphasis is the chrome layer's job, not this pure compute step.

/// Pane-rect geometry: tile a layout tree into per-pane rectangles.
pub mod layout;
/// Mouse hit-testing: map a click to the pane (and divider) under it.
pub mod mouse;
/// Rasterize the composition (pane interiors + divider segments) for paint.
pub mod rasterize;

pub use layout::{
    PaneLayout, compute_layout_in, pane_rects, pane_rects_in, pane_rects_proportional_in,
    split_content_span_at,
};
pub use mouse::{RouteDecision, route_mouse_event};
pub use rasterize::{DividerCell, DividerHit};

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::unnested_or_patterns,
    reason = "tests"
)]
mod tests {
    use super::*;
    use crate::layout::{LayoutNode, LayoutState, Rect, SplitDir, split_at};
    use crate::multi_pane::layout::compute_layout;
    use phux_protocol::ResourceId;
    use phux_protocol::input::key::ModSet;
    use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};

    fn t(id: u32) -> ResourceId {
        ResourceId::local(id)
    }

    fn leaf(id: u32) -> LayoutNode {
        LayoutNode::Leaf(t(id))
    }

    const fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }

    /// Panes 1 and 2 split along `dir` at `ratio`.
    fn pair(dir: SplitDir, ratio: f32) -> LayoutNode {
        split_at(&leaf(1), &t(1), &t(2), dir, ratio).unwrap()
    }

    fn focused(tree: LayoutNode, focus: u32) -> LayoutState {
        LayoutState {
            tree: Some(tree),
            focus: Some(t(focus)),
        }
    }

    /// `(1 | (2 | 3))`, both ratios 0.5.
    fn nested() -> LayoutNode {
        split_at(
            &pair(SplitDir::Horizontal, 0.5),
            &t(2),
            &t(3),
            SplitDir::Horizontal,
            0.5,
        )
        .unwrap()
    }

    #[test]
    fn empty_and_single_pane_layouts() {
        let out = compute_layout(&LayoutState::default(), (80, 24));
        assert!(out.dividers.is_empty());
        assert!(out.rects.is_empty());
        let out = compute_layout(&LayoutState::single(t(1)), (80, 24));
        assert!(out.dividers.is_empty());
        assert_eq!(out.rects.len(), 1);
        assert_eq!(out.rects[&t(1)], rect(0, 0, 80, 24));
    }

    #[test]
    fn compute_layout_in_insets_rects_and_dividers_by_content_origin() {
        let state = focused(pair(SplitDir::Horizontal, 0.5), 1);
        // Reserve a 20-col left sidebar: panes tile into x=20..80.
        let out = compute_layout_in(&state, rect(20, 0, 60, 24), (80, 24));
        let (ra, rb) = (out.rects[&t(1)], out.rects[&t(2)]);
        assert_eq!(ra.x, 20, "left pane inset by the sidebar width");
        assert_eq!(rb.x, ra.x + ra.w + 1, "right pane past the divider");
        assert_eq!(ra.w + rb.w + 1, 60);
        // Dividers fall inside the content area, never in the reserved zone.
        assert!(!out.dividers.is_empty());
        assert!(out.dividers.iter().all(|cell| (20..80).contains(&cell.x)));

        let rects = pane_rects_in(&pair(SplitDir::Vertical, 0.5), rect(0, 5, 80, 19));
        assert_eq!(rects.len(), 2);
        assert!(rects.values().all(|r| r.y >= 5), "pane above the top inset");
    }

    /// Three panes, two divider columns: both are the same light glyph,
    /// and neither changes when focus moves.
    #[test]
    fn a_focus_move_does_not_change_any_glyph() {
        let three = split_at(
            &pair(SplitDir::Horizontal, 0.5),
            &t(2),
            &t(3),
            SplitDir::Horizontal,
            0.5,
        )
        .unwrap();
        let with_1 = compute_layout(&focused(three.clone(), 1), (80, 24));
        let with_3 = compute_layout(&focused(three, 3), (80, 24));
        assert_eq!(with_1.dividers, with_3.dividers);
        let cols: std::collections::BTreeSet<u16> = with_1.dividers.iter().map(|c| c.x).collect();
        assert_eq!(cols.len(), 2, "expected two divider columns: {cols:?}");
        assert!(with_1.dividers.iter().all(|c| c.ch == '\u{2502}'));
    }

    /// Labelled-grid snapshots: a 2-pane split with either focus (the layout
    /// is identical), and a 3-pane cross split whose T-junctions must render.
    #[test]
    fn layout_snapshots() {
        let cross = split_at(
            &pair(SplitDir::Horizontal, 0.5),
            &t(1),
            &t(3),
            SplitDir::Vertical,
            0.5,
        )
        .unwrap();
        for (name, state) in [
            (
                "two_pane_h_split_80x24_focus_left",
                focused(pair(SplitDir::Horizontal, 0.5), 1),
            ),
            (
                "two_pane_h_split_80x24_focus_right",
                focused(pair(SplitDir::Horizontal, 0.5), 2),
            ),
            ("three_pane_cross_80x24_focus_top_left", focused(cross, 1)),
        ] {
            let layout = compute_layout(&state, (80, 24));
            insta::assert_snapshot!(name, render_layout_to_grid(&layout, 80, 24));
        }
    }

    /// Render a `PaneLayout` as a grid: pane interiors carry a per-pane
    /// letter (`a` + local id), divider cells their glyph.
    fn render_layout_to_grid(layout: &PaneLayout, cols: u16, rows: u16) -> String {
        let mut grid = vec![vec![' '; usize::from(cols)]; usize::from(rows)];
        for (id, r) in &layout.rects {
            let ResourceId::Local { id: n } = id else {
                unreachable!("tests build local panes")
            };
            let ch = char::from(b'a' + u8::try_from(*n).unwrap());
            for y in r.y..r.y.saturating_add(r.h).min(rows) {
                for x in r.x..r.x.saturating_add(r.w).min(cols) {
                    grid[usize::from(y)][usize::from(x)] = ch;
                }
            }
        }
        for cell in &layout.dividers {
            if cell.y < rows && cell.x < cols {
                grid[usize::from(cell.y)][usize::from(cell.x)] = cell.ch;
            }
        }
        grid.into_iter()
            .map(|row| row.into_iter().chain(['\n']).collect::<String>())
            .collect()
    }

    /// A ratio that would squeeze the right pane below its 2-col floor
    /// freezes it there; the deficit goes back to the left pane. The
    /// proportional view (the ADR-0019 resize gate's input) still reports
    /// the sub-minimum width. The row floor is 1.
    #[test]
    fn freeze_holds_squeezed_panes_at_their_floor_and_redistributes() {
        let tree = pair(SplitDir::Horizontal, 0.9);
        // Viewport 12 wide: content = 11, proportional cut = 10/1.
        let frozen = pane_rects(&tree, (12, 24));
        assert_eq!(frozen[&t(2)].w, 2, "right pane frozen at floor");
        assert_eq!(frozen[&t(1)].w, 9, "left pane absorbs the deficit");
        let proportional = pane_rects_proportional_in(&tree, rect(0, 0, 12, 24));
        assert_eq!(proportional[&t(2)].w, 1, "the raw ratio asks for less");

        // Viewport 5 tall: content = 4, proportional cut = 4/0.
        let rows = pane_rects(&pair(SplitDir::Vertical, 0.9), (80, 5));
        assert_eq!(rows[&t(2)].h, 1, "bottom pane frozen at 1 row");
        assert_eq!(rows[&t(1)].h, 3);
    }

    /// Nested splits: the root clamp reserves the right subtree's
    /// aggregate minimum (two 2-col leaves + one divider = 5), then the
    /// inner split holds both grandchildren at their floor.
    #[test]
    fn freeze_reserves_aggregate_minimums_through_nested_splits() {
        // Aggregate minimum width: 2 + 1 + (2 + 1 + 2) = 8; viewport 9.
        let rects = pane_rects(&nested(), (9, 24));
        assert_eq!(rects[&t(1)].w, 3, "spare cell goes left");
        assert_eq!(rects[&t(2)].w, 2);
        assert_eq!(rects[&t(3)].w, 2);
        assert_eq!(rects.values().map(|r| r.w).sum::<u16>(), 7);
    }

    /// Below the aggregate minimums the clamp disengages: frozen and
    /// proportional tilings agree, sub-viable rects appear, and the
    /// exact-tiling invariant still holds (no panic, no hole).
    #[test]
    fn freeze_disengages_below_aggregate_minimum_viewport() {
        let tree = pair(SplitDir::Horizontal, 0.5);
        // Aggregate minimum width is 5; viewport 4 cannot fit it.
        let frozen = pane_rects_in(&tree, rect(0, 0, 4, 24));
        let proportional = pane_rects_proportional_in(&tree, rect(0, 0, 4, 24));
        assert_eq!(frozen, proportional, "degenerate fallback is proportional");
        assert!(frozen.values().any(|r| r.w < 2));
        assert_eq!(frozen.values().map(|r| r.w).sum::<u16>() + 1, 4);
    }

    /// The drag-span geometry ([`split_content_span_at`]) descends with
    /// the same freeze clamp the paint walk uses, so a grabbed inner
    /// divider's span matches the frozen tiling, not proportional bounds.
    #[test]
    fn freeze_drag_span_matches_frozen_tiling() {
        use crate::layout::{NodePath, NodeStep};
        // In a 9-wide viewport the root clamp gives the right subtree
        // x = 4..9 (width 5), not the proportional x = 5..9.
        let content = rect(0, 0, 9, 24);
        let inner = NodePath(vec![NodeStep::Right]);
        let (start, len) = split_content_span_at(&nested(), content, &inner).unwrap();
        assert_eq!(pane_rects_in(&nested(), content)[&t(2)].x, start);
        assert_eq!((start, len), (4, 4));
    }

    fn mouse(x: f64, y: f64) -> MouseEvent {
        MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::Left,
            mods: ModSet::empty(),
            x,
            y,
        }
    }

    /// Route a click at `(col, row)` with no chrome in an 80x24 viewport.
    fn route(state: &LayoutState, col: u16, row: u16) -> RouteDecision {
        let at = mouse(f64::from(col), f64::from(row));
        route_mouse_event(state, rect(0, 0, 80, 24), (80, 24), &at)
    }

    fn hit(target: u32, pane_x: u16, pane_y: u16, focus_changed: bool) -> RouteDecision {
        RouteDecision::Pane {
            target: t(target),
            pane_x: f64::from(pane_x),
            pane_y: f64::from(pane_y),
            focus_changed,
        }
    }

    /// A click in a pane forwards pane-local coordinates and moves focus
    /// only when that pane was unfocused; a single pane always hits.
    #[test]
    fn route_mouse_hits_panes_with_local_coordinates() {
        let state = focused(pair(SplitDir::Horizontal, 0.5), 1);
        assert_eq!(route(&state, 5, 3), hit(1, 5, 3, false));
        let right = compute_layout(&state, (80, 24)).rects[&t(2)];
        assert_eq!(route(&state, right.x + 2, right.y + 1), hit(2, 2, 1, true));
        assert_eq!(
            route(&LayoutState::single(t(1)), 40, 12),
            hit(1, 40, 12, false)
        );
        // Pre-attach race protection: the driver drops these.
        assert_eq!(
            route(&LayoutState::default(), 10, 5),
            RouteDecision::NoFocus
        );
        // An out-of-viewport click clamps into the edge pane.
        let far = route_mouse_event(&state, rect(0, 0, 80, 24), (80, 24), &mouse(1e3, 1e3));
        assert!(matches!(far, RouteDecision::Pane { target, .. } if target == t(2)));
    }

    /// Clicking exactly on the divider column resolves to the split that
    /// divider controls (ADR-0048 drag-to-resize grab target).
    #[test]
    fn route_mouse_divider_resolves_to_controlling_split() {
        let state = focused(pair(SplitDir::Horizontal, 0.5), 1);
        let left_w = compute_layout(&state, (80, 24)).rects[&t(1)].w;
        assert_eq!(
            route(&state, left_w, 10),
            RouteDecision::Divider {
                node_path: crate::layout::NodePath::root(),
                axis: SplitDir::Horizontal,
            }
        );
    }

    /// While zoomed, routing hit-tests `ArrangementDoc::render_group`, so a click
    /// over the hidden pane lands on the zoomed one.
    #[test]
    fn route_mouse_while_zoomed_targets_the_zoomed_pane() {
        use crate::layout::{ArrangementDoc, Group};
        let state = focused(pair(SplitDir::Horizontal, 0.5), 1);
        let right = compute_layout(&state, (80, 24)).rects[&t(2)];
        let workspace = ArrangementDoc {
            groups: vec![Group::new("1".to_owned(), state)],
            active: 0,
            slots: Vec::new(),
        };
        let render = workspace.render_group(Some(&t(1))).unwrap();
        assert!(matches!(
            route(&render, right.x + 2, right.y + 1),
            RouteDecision::Pane { target, focus_changed: false, .. } if target == t(1)
        ));
    }

    /// Regression: hit-testing uses the inset content rect the renderer
    /// paints, so the status-bar row is chrome, not the bottom pane.
    #[test]
    fn route_mouse_respects_status_bar_inset() {
        let state = focused(pair(SplitDir::Vertical, 0.5), 1);
        // The status bar reserves row 23: panes tile into rows 0..23.
        let content = rect(0, 0, 80, 23);
        let inset =
            |row: u16| route_mouse_event(&state, content, (80, 24), &mouse(10.0, f64::from(row)));
        // Full-viewport tiling mis-routes the status-bar row to t(2).
        assert!(
            matches!(route(&state, 10, 23), RouteDecision::Pane { target, .. } if target == t(2))
        );
        assert_eq!(inset(23), RouteDecision::Miss);
        let bottom = compute_layout_in(&state, content, (80, 24)).rects[&t(2)];
        assert_eq!(inset(bottom.y), hit(2, 10, 0, true));
    }

    use std::collections::HashSet;

    use proptest::prelude::*;

    use crate::layout::{kill_pane, leaves};

    #[derive(Debug, Clone, Copy)]
    enum Op {
        /// Split the most recent pane at this (possibly awkward) ratio.
        AddPane(f32),
        KillPaneAt(usize),
    }

    fn arb_op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (0.05_f32..0.95).prop_map(Op::AddPane),
            1 => (0_usize..16).prop_map(Op::KillPaneAt),
        ]
    }

    /// Fold random split/kill ops into a single tree, splitting the most
    /// recently added pane each time so deep nesting is reached quickly
    /// (the regime where the old global-vs-local divider math diverged).
    #[allow(clippy::needless_pass_by_value)]
    fn apply_ops(ops: Vec<Op>) -> LayoutNode {
        let mut next_id: u32 = 1;
        let first = ResourceId::local(next_id);
        next_id += 1;
        let mut tree = LayoutNode::Leaf(first.clone());
        let mut alive = vec![first];
        for op in ops {
            match op {
                Op::AddPane(ratio) => {
                    let new_pane = ResourceId::local(next_id);
                    next_id += 1;
                    let Some(target) = alive.last().cloned() else {
                        continue;
                    };
                    let dir = if next_id.is_multiple_of(2) {
                        SplitDir::Horizontal
                    } else {
                        SplitDir::Vertical
                    };
                    if let Ok(t) = split_at(&tree, &target, &new_pane, dir, ratio) {
                        tree = t;
                        alive.push(new_pane);
                    }
                }
                Op::KillPaneAt(idx) => {
                    if alive.len() < 2 {
                        continue;
                    }
                    let target = alive[idx % alive.len()].clone();
                    if let Ok(Some(t)) = kill_pane(&tree, &target) {
                        tree = t;
                        alive.retain(|p| *p != target);
                    }
                }
            }
        }
        tree
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

        /// For any tree and non-empty viewport, leaf rects and divider cells
        /// partition the viewport exactly: no gap, no overlap, at any depth.
        #[test]
        fn proptest_rects_and_dividers_tile_exactly(
            ops in prop::collection::vec(arb_op(), 1..18),
            cols in 1_u16..60,
            rows in 1_u16..30,
        ) {
            let tree = apply_ops(ops);
            let focus = leaves(&tree).into_iter().next();
            let state = LayoutState { tree: Some(tree), focus };

            let rects = pane_rects(state.tree.as_ref().unwrap(), (cols, rows));
            let layout = compute_layout(&state, (cols, rows));

            let mut covered: HashSet<(u16, u16)> = HashSet::new();

            // Leaf cells: pairwise-disjoint, all inside the viewport.
            for r in rects.values() {
                prop_assert!(u32::from(r.x) + u32::from(r.w) <= u32::from(cols));
                prop_assert!(u32::from(r.y) + u32::from(r.h) <= u32::from(rows));
                for y in r.y..r.y.saturating_add(r.h) {
                    for x in r.x..r.x.saturating_add(r.w) {
                        prop_assert!(covered.insert((x, y)), "leaf overlap at ({x}, {y})");
                    }
                }
            }

            // Divider cells: inside the viewport, disjoint from leaves and
            // from each other.
            for c in &layout.dividers {
                prop_assert!(
                    c.x < cols && c.y < rows,
                    "divider ({}, {}) outside {cols}x{rows} viewport", c.x, c.y
                );
                prop_assert!(
                    covered.insert((c.x, c.y)),
                    "divider overlaps a leaf or another divider at ({}, {})", c.x, c.y
                );
            }

            // Exact cover: nothing left uncovered.
            prop_assert_eq!(covered.len(), usize::from(cols) * usize::from(rows));

            // The union of divider hit cells is exactly the painted divider set, and
            // every hit's path resolves to a split (ADR-0048).
            let painted: HashSet<(u16, u16)> =
                layout.dividers.iter().map(|c| (c.x, c.y)).collect();
            let mut hit_cells: HashSet<(u16, u16)> = HashSet::new();
            for h in &layout.divider_hits {
                // The path must address a real split in the tree.
                let retuned = crate::layout::set_ratio_at(
                    state.tree.as_ref().unwrap(),
                    &h.node_path,
                    0.5,
                );
                prop_assert!(
                    retuned.is_some(),
                    "divider_hit node_path {:?} does not resolve to a Split", h.node_path
                );
                for cell in &h.cells {
                    prop_assert!(
                        painted.contains(cell),
                        "divider_hit cell {cell:?} is not a painted divider cell"
                    );
                    hit_cells.insert(*cell);
                }
            }
            prop_assert_eq!(
                &hit_cells, &painted,
                "divider_hits cells must equal painted divider cells"
            );
        }
    }
}
