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
    PaneLayout, compute_layout, compute_layout_in, pane_rects, pane_rects_in,
    pane_rects_proportional_in, split_content_span_at,
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
    use phux_protocol::ResourceId;
    use phux_protocol::input::key::ModSet;
    use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};

    fn t(id: u32) -> ResourceId {
        ResourceId::local(id)
    }

    fn leaf(id: u32) -> LayoutNode {
        LayoutNode::Leaf(t(id))
    }

    #[test]
    fn single_pane_no_dividers() {
        let state = LayoutState::single(t(1));
        let out = compute_layout(&state, (80, 24));
        assert!(out.dividers.is_empty());
        assert_eq!(out.rects.len(), 1);
        let r = out.rects.get(&t(1)).unwrap();
        assert_eq!(
            *r,
            Rect {
                x: 0,
                y: 0,
                w: 80,
                h: 24
            }
        );
    }

    #[test]
    fn empty_layout_returns_empty() {
        let state = LayoutState::default();
        let out = compute_layout(&state, (80, 24));
        assert!(out.dividers.is_empty());
        assert!(out.rects.is_empty());
    }

    #[test]
    fn compute_layout_in_insets_rects_and_dividers_by_content_origin() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        // Reserve a 20-col left sidebar: panes tile into x=20..80.
        let content = Rect {
            x: 20,
            y: 0,
            w: 60,
            h: 24,
        };
        let out = compute_layout_in(&state, content, (80, 24));
        let ra = out.rects.get(&t(1)).unwrap();
        let rb = out.rects.get(&t(2)).unwrap();
        // Left pane starts at the content origin, not the viewport origin.
        assert_eq!(ra.x, 20, "left pane inset by the sidebar width");
        assert_eq!(rb.x, ra.x + ra.w + 1, "right pane past the divider");
        // Panes + divider exactly fill the content width.
        assert_eq!(ra.w + rb.w + 1, 60);
        // Dividers fall inside the content area — never in the reserved zone.
        assert!(!out.dividers.is_empty());
        for cell in &out.dividers {
            assert!(cell.x >= 20, "divider escaped into the sidebar: {cell:?}");
            assert!(cell.x < 80, "divider escaped the screen: {cell:?}");
        }
    }

    #[test]
    fn compute_layout_in_full_viewport_matches_compute_layout() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Vertical, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        let full = Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 24,
        };
        assert_eq!(
            compute_layout_in(&state, full, (80, 24)),
            compute_layout(&state, (80, 24)),
            "content == full viewport is the identity case"
        );
    }

    #[test]
    fn pane_rects_in_offsets_every_leaf_by_the_top_inset() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Vertical, 0.5).unwrap();
        let content = Rect {
            x: 0,
            y: 5,
            w: 80,
            h: 19,
        };
        let rects = pane_rects_in(&tree, content);
        assert_eq!(rects.len(), 2);
        for r in rects.values() {
            assert!(r.y >= 5, "pane above the reserved top inset: {r:?}");
        }
    }

    /// Three panes, two divider columns: both are the same light glyph,
    /// and neither changes when focus moves.
    #[test]
    fn a_focus_move_does_not_change_any_glyph() {
        let t1 = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let t2 = split_at(&t1, &t(2), &t(3), SplitDir::Horizontal, 0.5).unwrap();
        let with_1 = compute_layout(
            &LayoutState {
                tree: Some(t2.clone()),
                focus: Some(t(1)),
            },
            (80, 24),
        );
        let with_3 = compute_layout(
            &LayoutState {
                tree: Some(t2),
                focus: Some(t(3)),
            },
            (80, 24),
        );
        assert_eq!(with_1.dividers, with_3.dividers);
        let cols: std::collections::BTreeSet<u16> = with_1.dividers.iter().map(|c| c.x).collect();
        assert_eq!(cols.len(), 2, "expected two divider columns: {cols:?}");
        for c in &with_1.dividers {
            assert_eq!(c.ch, '\u{2502}');
        }
    }

    /// A 2-pane horizontal split in 80x24, rendered as a labelled grid.
    #[test]
    fn snapshot_two_pane_horizontal_split_80x24_focus_left() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        let layout = compute_layout(&state, (80, 24));
        let grid = render_layout_to_grid(&layout, 80, 24);
        insta::assert_snapshot!("two_pane_h_split_80x24_focus_left", grid);
    }

    /// Same shape, focus on pane 2 — the heavy edge moves to the right
    /// of the divider but the layout is otherwise identical.
    #[test]
    fn snapshot_two_pane_horizontal_split_80x24_focus_right() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(2)),
        };
        let layout = compute_layout(&state, (80, 24));
        let grid = render_layout_to_grid(&layout, 80, 24);
        insta::assert_snapshot!("two_pane_h_split_80x24_focus_right", grid);
    }

    /// 3-pane mixed split: horizontal then vertical inside the left
    /// half. Tests that T-piece junctions render correctly and that
    /// focus chrome doesn't bleed across non-adjacent dividers.
    #[test]
    fn snapshot_three_pane_cross_split_80x24_focus_top_left() {
        let t1 = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let t2 = split_at(&t1, &t(1), &t(3), SplitDir::Vertical, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(t2),
            focus: Some(t(1)),
        };
        let layout = compute_layout(&state, (80, 24));
        let grid = render_layout_to_grid(&layout, 80, 24);
        insta::assert_snapshot!("three_pane_cross_80x24_focus_top_left", grid);
    }

    /// Render a `PaneLayout` as a grid: pane interiors carry a per-pane
    /// letter, divider cells their glyph.
    fn render_layout_to_grid(layout: &PaneLayout, cols: u16, rows: u16) -> String {
        let mut grid: Vec<Vec<char>> = (0..rows).map(|_| vec![' '; cols as usize]).collect();
        // Paint pane interiors first.
        for (id, r) in &layout.rects {
            let ch = pane_glyph(id);
            for y in r.y..r.y.saturating_add(r.h).min(rows) {
                for x in r.x..r.x.saturating_add(r.w).min(cols) {
                    grid[y as usize][x as usize] = ch;
                }
            }
        }
        // Then the dividers (overwriting any interior cell that
        // happened to overlap; in a well-formed layout this never
        // happens, but `.min()` defends against degenerate inputs).
        for cell in &layout.dividers {
            if (cell.y as usize) < grid.len() && (cell.x as usize) < grid[0].len() {
                grid[cell.y as usize][cell.x as usize] = cell.ch;
            }
        }
        let mut out = String::with_capacity(grid.len() * (cols as usize + 1));
        for row in grid {
            for c in row {
                out.push(c);
            }
            out.push('\n');
        }
        out
    }

    fn pane_glyph(id: &ResourceId) -> char {
        // Map ResourceId::Local { id: N } to the lowercase letter a + N
        // for N < 26; otherwise the digit 0–9. Tests only construct
        // small N so the letter form is always hit.
        match id {
            ResourceId::Local { id: n } => {
                if *n < 26 {
                    char::from(b'a' + u8::try_from(*n).unwrap_or(0))
                } else {
                    char::from(b'0' + u8::try_from(*n % 10).unwrap_or(0))
                }
            }
            ResourceId::Satellite { .. } => '?',
        }
    }

    /// A ratio that would squeeze the right pane below its 2-col floor
    /// freezes it there; the deficit goes back to the left pane. The
    /// proportional view (what the ratio asks for; the ADR-0019 resize
    /// gate's input) still reports the sub-minimum width.
    #[test]
    fn freeze_holds_squeezed_pane_at_min_cols_and_redistributes() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.9).unwrap();
        // Viewport 12 wide: content = 11, proportional cut = 10/1.
        let frozen = pane_rects(&tree, (12, 24));
        assert_eq!(
            frozen.get(&t(2)).unwrap().w,
            2,
            "right pane frozen at floor"
        );
        assert_eq!(
            frozen.get(&t(1)).unwrap().w,
            9,
            "left pane absorbs the deficit"
        );

        let proportional = pane_rects_proportional_in(
            &tree,
            Rect {
                x: 0,
                y: 0,
                w: 12,
                h: 24,
            },
        );
        assert_eq!(
            proportional.get(&t(2)).unwrap().w,
            1,
            "the raw ratio still asks for a sub-minimum pane"
        );
    }

    /// Nested splits: the root clamp reserves the right subtree's
    /// aggregate minimum (two 2-col leaves + one divider = 5), then the
    /// inner split holds both grandchildren at their floor. Every leaf
    /// keeps >= 2 cols; the leftover cell lands on the lone left leaf.
    #[test]
    fn freeze_reserves_aggregate_minimums_through_nested_splits() {
        // (1 | (2 | 3)), both ratios 0.5.
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let tree = split_at(&tree, &t(2), &t(3), SplitDir::Horizontal, 0.5).unwrap();
        // Aggregate minimum width: 2 + 1 + (2 + 1 + 2) = 8; viewport 9.
        let rects = pane_rects(&tree, (9, 24));
        assert_eq!(rects.get(&t(1)).unwrap().w, 3, "spare cell goes left");
        assert_eq!(rects.get(&t(2)).unwrap().w, 2);
        assert_eq!(rects.get(&t(3)).unwrap().w, 2);
        // Exact tiling: 3 + divider + 2 + divider + 2 = 9.
        let total: u16 = rects.values().map(|r| r.w).sum();
        assert_eq!(total, 7);
    }

    /// The row floor is 1 (not 2): a vertical squeeze freezes the bottom
    /// pane at one row.
    #[test]
    fn freeze_holds_squeezed_pane_at_min_one_row() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Vertical, 0.9).unwrap();
        // Viewport 5 tall: content = 4, proportional cut = 4/0.
        let rects = pane_rects(&tree, (80, 5));
        assert_eq!(
            rects.get(&t(2)).unwrap().h,
            1,
            "bottom pane frozen at 1 row"
        );
        assert_eq!(rects.get(&t(1)).unwrap().h, 3);
    }

    /// Below the aggregate minimums the clamp disengages: frozen and
    /// proportional tilings agree, sub-viable rects appear, and the
    /// exact-tiling invariant still holds (no panic, no hole).
    #[test]
    fn freeze_disengages_below_aggregate_minimum_viewport() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        // Aggregate minimum width is 5; viewport 4 cannot fit it.
        let content = Rect {
            x: 0,
            y: 0,
            w: 4,
            h: 24,
        };
        let frozen = pane_rects_in(&tree, content);
        let proportional = pane_rects_proportional_in(&tree, content);
        assert_eq!(frozen, proportional, "degenerate fallback is proportional");
        assert!(
            frozen.values().any(|r| r.w < 2),
            "some pane is sub-viable in the degenerate regime"
        );
        let total: u16 = frozen.values().map(|r| r.w).sum();
        assert_eq!(total + 1, 4, "leaves + divider still tile exactly");
    }

    /// The drag-span geometry ([`split_content_span_at`]) descends with
    /// the same freeze clamp the paint walk uses, so a grabbed inner
    /// divider's span matches the frozen tiling — not the proportional
    /// child bounds the unclamped math would produce.
    #[test]
    fn freeze_drag_span_matches_frozen_tiling() {
        use crate::layout::{NodePath, NodeStep};
        // (1 | (2 | 3)) in a 9-wide viewport: the root clamp gives the
        // right subtree x = 4..9 (width 5), not the proportional x = 5..9.
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let tree = split_at(&tree, &t(2), &t(3), SplitDir::Horizontal, 0.5).unwrap();
        let content = Rect {
            x: 0,
            y: 0,
            w: 9,
            h: 24,
        };
        let inner = NodePath(vec![NodeStep::Right]);
        let (start, len) = split_content_span_at(&tree, content, &inner).unwrap();
        // Frozen tiling: pane 2 paints at x = 4 (see the frozen rects), so
        // the inner split's span starts there with a 4-cell budget
        // (5 minus its own divider).
        let rects = pane_rects_in(&tree, content);
        assert_eq!(rects.get(&t(2)).unwrap().x, start);
        assert_eq!(start, 4);
        assert_eq!(len, 4);
    }

    fn mouse_at(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::Left,
            mods: ModSet::empty(),
            x: f64::from(col),
            y: f64::from(row),
        }
    }

    /// A content rect spanning the whole viewport — the no-chrome case where
    /// the hit-test layout equals the full viewport.
    fn full_content(cols: u16, rows: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: cols,
            h: rows,
        }
    }

    /// Clicking inside the focused pane forwards with focus unchanged
    /// and emits pane-local coordinates relative to the pane's `Rect`.
    #[test]
    fn route_mouse_inside_focused_pane_no_focus_change() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        // Click inside the left half (focused) at col 5, row 3.
        let decision = route_mouse_event(&state, full_content(80, 24), (80, 24), &mouse_at(5, 3));
        match decision {
            RouteDecision::Pane {
                target,
                pane_x,
                pane_y,
                focus_changed,
            } => {
                assert_eq!(target, t(1));
                assert!(!focus_changed);
                // Left pane sits at x=0; pane-local matches outer.
                assert!((pane_x - 5.0).abs() < f64::EPSILON);
                assert!((pane_y - 3.0).abs() < f64::EPSILON);
            }
            other => panic!("expected Pane decision, got {other:?}"),
        }
    }

    /// Clicking in a non-focused pane reports `focus_changed` and
    /// returns pane-local coordinates relative to that pane's `Rect`.
    #[test]
    fn route_mouse_click_other_pane_updates_focus() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        let layout = compute_layout(&state, (80, 24));
        let right_rect = layout.rects.get(&t(2)).copied().unwrap();
        // Click 2 cells into the right pane, on the second row.
        let click_x = right_rect.x + 2;
        let click_y = right_rect.y + 1;
        let decision = route_mouse_event(
            &state,
            full_content(80, 24),
            (80, 24),
            &mouse_at(click_x, click_y),
        );
        match decision {
            RouteDecision::Pane {
                target,
                pane_x,
                pane_y,
                focus_changed,
            } => {
                assert_eq!(target, t(2));
                assert!(focus_changed, "click in unfocused pane must move focus");
                assert!((pane_x - 2.0).abs() < f64::EPSILON);
                assert!((pane_y - 1.0).abs() < f64::EPSILON);
            }
            other => panic!("expected Pane decision, got {other:?}"),
        }
    }

    /// Clicking exactly on the divider column resolves to the split that
    /// divider controls (ADR-0048 drag-to-resize grab target).
    #[test]
    fn route_mouse_divider_resolves_to_controlling_split() {
        use crate::layout::{NodePath, SplitDir as Sd};
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        let layout = compute_layout(&state, (80, 24));
        // The divider sits at the column equal to the left pane's
        // width (its `Rect.w`). Any row on that column hits the gap.
        let left_w = layout.rects.get(&t(1)).copied().unwrap().w;
        let decision = route_mouse_event(
            &state,
            full_content(80, 24),
            (80, 24),
            &mouse_at(left_w, 10),
        );
        match decision {
            RouteDecision::Divider { node_path, axis } => {
                // Root split (the only split) controls this divider; its
                // axis is Horizontal (a vertical line moved left/right).
                assert_eq!(node_path, NodePath::root());
                assert_eq!(axis, Sd::Horizontal);
            }
            other => panic!("expected Divider decision, got {other:?}"),
        }
    }

    /// Single-pane layout: every click stays in the lone pane with no
    /// focus change, regardless of position.
    #[test]
    fn route_mouse_single_pane_always_hits() {
        let state = LayoutState::single(t(1));
        let decision = route_mouse_event(&state, full_content(80, 24), (80, 24), &mouse_at(40, 12));
        match decision {
            RouteDecision::Pane {
                target,
                focus_changed,
                pane_x,
                pane_y,
            } => {
                assert_eq!(target, t(1));
                assert!(!focus_changed);
                assert!((pane_x - 40.0).abs() < f64::EPSILON);
                assert!((pane_y - 12.0).abs() < f64::EPSILON);
            }
            other => panic!("expected Pane, got {other:?}"),
        }
    }

    /// While zoomed, routing hit-tests `Workspace::render_window`, so a click
    /// over the hidden pane lands on the zoomed one.
    #[test]
    fn route_mouse_while_zoomed_targets_the_zoomed_pane() {
        use crate::layout::{WindowState, Workspace};
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let workspace = Workspace {
            windows: vec![WindowState::new(
                "1".to_owned(),
                LayoutState {
                    tree: Some(tree),
                    focus: Some(t(1)),
                },
            )],
            active: 0,
        };
        // In the real tiled tree pane t(2) owns the right half; a click
        // there would normally route to t(2).
        let tiled = compute_layout(workspace.active_window().unwrap(), (80, 24));
        let right_rect = tiled.rects.get(&t(2)).copied().unwrap();
        let click_x = right_rect.x + 2;
        let click_y = right_rect.y + 1;
        // Zoom pane t(1): the render layout collapses to a single leaf, so
        // the same click lands on t(1) instead.
        let render = workspace.render_window(Some(&t(1))).unwrap();
        let decision = route_mouse_event(
            &render,
            full_content(80, 24),
            (80, 24),
            &mouse_at(click_x, click_y),
        );
        match decision {
            RouteDecision::Pane {
                target,
                focus_changed,
                ..
            } => {
                assert_eq!(target, t(1), "click while zoomed must hit the zoomed pane");
                assert!(!focus_changed, "zoomed single-leaf is already focused");
            }
            other => panic!("expected Pane, got {other:?}"),
        }
    }

    /// Empty layout (no tree) returns `NoFocus`. Pre-attach race
    /// protection — the driver drops these.
    #[test]
    fn route_mouse_empty_layout_returns_no_focus() {
        let state = LayoutState::default();
        let decision = route_mouse_event(&state, full_content(80, 24), (80, 24), &mouse_at(10, 5));
        assert_eq!(decision, RouteDecision::NoFocus);
    }

    /// An out-of-viewport click clamps into the edge pane.
    #[test]
    fn route_mouse_out_of_range_clamps_into_edge_pane() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        let decision = route_mouse_event(
            &state,
            full_content(80, 24),
            (80, 24),
            &MouseEvent {
                action: MouseAction::Press,
                button: MouseButton::Left,
                mods: ModSet::empty(),
                x: 1_000.0,
                y: 1_000.0,
            },
        );
        // Only the right pane reaches column 79.
        if let RouteDecision::Pane { target, .. } = decision {
            assert_eq!(target, t(2));
        } else {
            panic!("expected Pane decision, got {decision:?}");
        }
    }

    /// Regression: hit-testing uses the inset content rect the renderer
    /// paints, so the status-bar row is chrome, not the bottom pane.
    #[test]
    fn route_mouse_respects_status_bar_inset() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Vertical, 0.5).unwrap();
        let state = LayoutState {
            tree: Some(tree),
            focus: Some(t(1)),
        };
        let viewport = (80, 24);
        // Status bar reserves the last row: panes tile into rows 0..23.
        let content = Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 23,
        };
        let status_row = 23; // the bottom row, owned by the status bar

        // The bug: hit-testing the status-bar row against the FULL viewport
        // routes it to the bottom pane, which paints down to row 23 there.
        let buggy = route_mouse_event(
            &state,
            full_content(80, 24),
            viewport,
            &mouse_at(10, status_row),
        );
        assert!(
            matches!(&buggy, RouteDecision::Pane { target, .. } if *target == t(2)),
            "test premise: full-viewport tiling mis-routes the status-bar row to t(2), got {buggy:?}"
        );

        // The fix: hit-testing against the inset content drops the chrome click.
        let fixed = route_mouse_event(&state, content, viewport, &mouse_at(10, status_row));
        assert_eq!(
            fixed,
            RouteDecision::Miss,
            "a click on the reserved status-bar row must be dropped"
        );

        // A click genuinely inside the painted bottom pane still routes to t(2).
        let bottom = compute_layout_in(&state, content, viewport)
            .rects
            .get(&t(2))
            .copied()
            .unwrap();
        match route_mouse_event(&state, content, viewport, &mouse_at(10, bottom.y)) {
            RouteDecision::Pane {
                target,
                pane_y,
                focus_changed,
                ..
            } => {
                assert_eq!(target, t(2), "click in painted bottom pane hits t(2)");
                assert!(focus_changed);
                assert!((pane_y - 0.0).abs() < f64::EPSILON);
            }
            other => panic!("expected Pane decision, got {other:?}"),
        }
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
