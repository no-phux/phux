use super::*;

mod fixture;
mod ime;
mod input;
mod refit;
mod touch;
mod viewport;

fn id(n: u32) -> ResourceId {
    ResourceId::local(n)
}

fn split(dir: SplitDir, ratio: f32, left: LayoutNode, right: LayoutNode) -> LayoutNode {
    LayoutNode::Split {
        dir,
        ratio,
        left: Box::new(left),
        right: Box::new(right),
    }
}

fn tree() -> LayoutNode {
    split(
        SplitDir::Horizontal,
        0.25,
        split(
            SplitDir::Vertical,
            0.5,
            LayoutNode::Leaf(id(1)),
            LayoutNode::Leaf(id(2)),
        ),
        split(
            SplitDir::Vertical,
            0.75,
            LayoutNode::Leaf(id(3)),
            LayoutNode::Leaf(id(4)),
        ),
    )
}

#[test]
fn unequal_nested_splits_share_exact_geometry_and_four_directions() {
    let tree = tree();
    assert_eq!(
        rects(&tree),
        vec![
            (
                id(1),
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 0.25,
                    h: 0.5
                }
            ),
            (
                id(2),
                Rect {
                    x: 0.0,
                    y: 0.5,
                    w: 0.25,
                    h: 0.5
                }
            ),
            (
                id(3),
                Rect {
                    x: 0.25,
                    y: 0.0,
                    w: 0.75,
                    h: 0.75
                }
            ),
            (
                id(4),
                Rect {
                    x: 0.25,
                    y: 0.75,
                    w: 0.75,
                    h: 0.25
                }
            ),
        ]
    );
    assert_eq!(toward(&tree, &id(1), Direction::Right), Some(id(3)));
    assert_eq!(toward(&tree, &id(1), Direction::Down), Some(id(2)));
    assert_eq!(toward(&tree, &id(4), Direction::Left), Some(id(2)));
    assert_eq!(toward(&tree, &id(4), Direction::Up), Some(id(3)));
    assert_eq!(toward(&tree, &id(1), Direction::Left), None);
    assert_eq!(toward(&tree, &id(1), Direction::Up), None);
    assert_eq!(
        camera(rects(&tree)[3].1, [300.0, 400.0]),
        [400.0, 1600.0, -100.0, -1200.0]
    );
}

#[test]
fn map_hit_boundaries_never_overlap_and_outside_hits_nothing() {
    let tree = tree();
    assert_eq!(hit(&tree, 0.25, 0.75), Some(id(4)));
    assert_eq!(hit(&tree, 0.249, 0.5), Some(id(2)));
    assert_eq!(hit(&tree, -0.01, 0.5), None);
    assert_eq!(hit(&tree, 1.0, 1.0), None);
    assert_eq!(hit(&tree, f32::NAN, 0.5), None);
}

#[test]
fn collapsed_and_invalid_ratios_keep_finite_cameras_and_literal_zero_areas() {
    for ratio in [0.0, 1.0, f32::NAN, f32::INFINITY] {
        let tree = split(
            SplitDir::Horizontal,
            ratio,
            LayoutNode::Leaf(id(1)),
            LayoutNode::Leaf(id(2)),
        );
        for (_, r) in rects(&tree) {
            assert!(camera(r, [393.0, 400.0]).iter().all(|v| v.is_finite()));
        }
    }
    let tree = split(
        SplitDir::Vertical,
        0.0,
        LayoutNode::Leaf(id(1)),
        LayoutNode::Leaf(id(2)),
    );
    assert_eq!(rects(&tree)[0].1.h, 0.0);
    assert_eq!(hit(&tree, 0.5, 0.0), Some(id(2)));
}

#[test]
fn session_identity_change_resets_navigation_without_changing_the_tree() {
    use crate::desk::phone::Phone;
    use phux_protocol::ids::SessionKey;
    let mut phone = Phone::default();
    phone.reconcile(
        Some(tree()),
        Some((SessionKey::new([1; 16]).unwrap(), [8; 16])),
    );
    phone.navigation = Some((id(1), true));
    phone.travel = [-80.0, 0.0];
    phone.picker = true;
    phone.reconcile(
        Some(tree()),
        Some((SessionKey::new([2; 16]).unwrap(), [8; 16])),
    );
    assert!(!phone.navigating());
    assert!(!phone.picker);
    assert_eq!(phone.travel, [0.0; 2]);
}

use fixture::*;
use rune::TouchPhase::{Cancelled, Ended, Moved, Started};

#[test]
fn real_map_taps_choose_literal_nested_leaves_and_track_external_focus() {
    let (client, mut rt) = fixture(true);
    let arrangement_before = stored(&client);
    assert_eq!(focus(&rt), id(1));
    assert_eq!(rt.state.on_screen(), [id(1)]);
    assert_eq!(rt.state.others.len(), 3);
    assert!(rt.state.others[&id(2)].is_some());
    assert_eq!(rt.state.others[&id(4)], None, "document shares the map");
    let [x, y, w, h] = geometry(&rt, "phone-minimap").rect;
    touch(&mut rt, Started, x + w * 0.6, y + h * 0.3);
    touch(&mut rt, Ended, x + w * 0.6, y + h * 0.3);
    assert_eq!(focus(&rt), id(3));
    let marker_in_flight = geometry(&rt, "phone-map-focus");
    assert_ne!(
        marker_in_flight.rect, marker_in_flight.target,
        "one stable viewport marker springs to the new focus"
    );
    settle(&mut rt);
    let current = geometry(&rt, "phone-map-focus");
    rt.update(|d, _| d.want(id(2)));
    poll(&mut rt);
    settle(&mut rt);
    assert_eq!(focus(&rt), id(2));
    let marker = geometry(&rt, "phone-map-focus");
    assert_ne!(marker.rect, current.rect);
    assert_eq!(
        marker.rect,
        geometry(&rt, &format!("phone-map-pane-{}", id(2))).rect
    );
    assert_eq!(
        stored(&client),
        arrangement_before,
        "camera/focus never rewrite ratios or leaf order"
    );
    no_input(&client);
}

#[test]
fn a_navigation_drag_exposes_real_neighbors_then_springs_to_one_destination() {
    let (client, mut rt) = fixture(true);
    let initial = geometry(&rt, "phone-canvas").rect;
    let next_key = format!("phone-spatial-pane-{}", id(3));
    let hidden = geometry(&rt, &next_key).visible;
    let (x, y) = rt.find("phone-navigation").unwrap();
    touch(&mut rt, Started, x, y);
    touch(&mut rt, Moved, x - 80.0, y);
    let dragged = geometry(&rt, "phone-canvas").rect;
    assert!(
        dragged[0] < initial[0] - 60.0,
        "camera follows finger: {dragged:?} vs {initial:?}"
    );
    assert!(
        geometry(&rt, &next_key).visible[2] > hidden[2] + 40.0,
        "adjacent pane revealed"
    );
    assert_eq!(focus(&rt), id(1), "no focus commit before release");
    touch(&mut rt, Ended, x - 80.0, y);
    assert_eq!(focus(&rt), id(3));
    let moving = geometry(&rt, "phone-canvas");
    assert_ne!(
        moving.rect, moving.target,
        "real intermediate camera geometry, not just final focus"
    );
    assert!(rt.animating);
    settle(&mut rt);
    let landed = geometry(&rt, &next_key);
    let stage = geometry(&rt, "phone-spatial-stage");
    for axis in 0..4 {
        assert!(
            (landed.rect[axis] - stage.rect[axis]).abs() < 1.0,
            "focused pane fills stage: {landed:?}"
        );
    }
    assert_eq!(landed.scale, 1.0, "glyphs never scaled");
    no_input(&client);
}

#[test]
fn cancelled_slop_boundary_and_second_finger_never_commit_a_neighbor() {
    let (client, mut rt) = fixture(true);
    let initial = geometry(&rt, "phone-canvas").rect;
    let (x, y) = rt.find("phone-navigation").unwrap();
    touch(&mut rt, Started, x, y);
    touch(&mut rt, Moved, x - 70.0, y);
    touch(&mut rt, Cancelled, x - 70.0, y);
    assert_eq!(focus(&rt), id(1));
    assert!(rt.state.phone.as_ref().unwrap().travel == [0.0; 2]);
    settle(&mut rt);
    assert_eq!(geometry(&rt, "phone-canvas").rect, initial);
    for dx in [-4.0, -20.0, 90.0] {
        touch(&mut rt, Started, x, y);
        touch(&mut rt, Moved, x + dx, y);
        touch(&mut rt, Ended, x + dx, y);
        assert_eq!(focus(&rt), id(1), "slop or left boundary: {dx}");
        settle(&mut rt);
        assert_eq!(geometry(&rt, "phone-canvas").rect, initial);
    }
    touch(&mut rt, Started, x, y);
    rt.touch(2, Started, x, y, 2.0);
    rt.touch(2, Moved, x - 90.0, y, 2.1);
    rt.touch(2, Ended, x - 90.0, y, 2.2);
    touch(&mut rt, Ended, x, y);
    assert_eq!(focus(&rt), id(1));
    no_input(&client);
}

#[test]
fn map_swipes_do_not_also_tap_and_picker_reaches_document_and_tiny_leaves() {
    let (client, mut rt) = fixture(true);
    let (x, y) = rt.find("phone-minimap").unwrap();
    touch(&mut rt, Started, x, y);
    touch(&mut rt, Moved, x, y - 45.0);
    touch(&mut rt, Ended, x, y - 45.0);
    assert_eq!(focus(&rt), id(2), "one down neighbor, not a map tap to 3");
    tap(&mut rt, "phone-pane-picker");
    settle(&mut rt);
    tap(&mut rt, &format!("phone-pick-{}", id(4)));
    assert_eq!(focus(&rt), id(4));
    assert!(rt.state.shown.as_ref().unwrap().view.is_none());
    assert!(!rt.state.phone.as_ref().unwrap().picker);
    no_input(&client);
}

#[test]
fn modal_blocks_map_handle_picker_and_tabs_without_terminal_input() {
    let (client, mut rt) = fixture(true);
    rt.update(|d, _| d.pending = Some(crate::desk::Pending::Close(id(1))));
    settle(&mut rt);
    for key in [
        "phone-minimap",
        "phone-navigation",
        "phone-pane-picker",
        "phone-tab-1",
    ] {
        tap(&mut rt, key);
        assert_eq!(focus(&rt), id(1), "blocked {key}");
    }
    assert!(!rt.state.phone.as_ref().unwrap().picker);
    assert!(rt.state.tabs.list[0].active);
    no_input(&client);
}

#[test]
fn returning_keeps_real_views_scroll_selection_and_only_focused_pane_is_fit_or_seen() {
    use phux_client_runtime::control::Event;
    let (client, mut rt) = fixture(true);
    let first = rt.state.shown.as_ref().unwrap().view.unwrap();
    let second = rt.state.others[&id(2)].unwrap();
    let [x, y, _, _] = geometry(&rt, &format!("phone-spatial-pane-{}", id(1))).rect;
    touch(&mut rt, Started, x + 60.0, y + 100.0);
    touch(&mut rt, Moved, x + 60.0, y + 180.0);
    touch(&mut rt, Ended, x + 60.0, y + 180.0);
    poll(&mut rt);
    let offset = client.acquire_view(first).unwrap().scrollbar.offset;
    assert!(
        !client.acquire_view(first).unwrap().scrollbar.at_tail(),
        "positive wheel means older history"
    );
    let engine = client.engine().unwrap();
    assert!(engine.select_all_view(first).unwrap());
    let selection = engine.view_selection_text(first).unwrap();
    assert!(!selection.is_empty());
    rt.update(|d, tx| {
        d.apply(&client, Event::Bell { terminal_id: id(2) }, tx);
    });
    rt.update(|d, _| {
        d.refresh_layout(&client);
    });
    assert_eq!(
        rt.state.activities.mark(&id(2)),
        crate::activity::Mark::Bell,
        "painting did not mark hidden pane seen"
    );
    assert_eq!(rt.state.fitted.len(), 1);
    assert_eq!(rt.state.areas.len(), 1);
    assert!(
        rt.state
            .painted
            .borrow()
            .as_ref()
            .is_none_or(|p| p.terminal == id(1))
    );
    rt.update(|d, _| d.focus_pane(&id(2)));
    settle(&mut rt);
    assert_eq!(rt.state.shown.as_ref().unwrap().view, Some(second));
    assert_eq!(rt.state.others[&id(1)], Some(first));
    rt.update(|d, _| d.focus_pane(&id(1)));
    settle(&mut rt);
    assert_eq!(rt.state.shown.as_ref().unwrap().view, Some(first));
    assert_eq!(client.acquire_view(first).unwrap().scrollbar.offset, offset);
    assert_eq!(engine.view_selection_text(first).unwrap(), selection);
    no_input(&client);
}

#[test]
fn keyboard_resizes_only_focus_and_drag_frames_do_not_storm_pty_sizes() {
    use phux_protocol::wire::frame::FrameKind;
    let (client, mut rt) = fixture(true);
    // Unlike the geometry-only fixture, resize settling runs on the app clock.
    rt.poll = Some(Desk::poll);
    let before = rt.state.fitted[&id(1)];
    tap(&mut rt, "phone-keyboard-toggle");
    settle(&mut rt);
    assert!(!rt.state.phone.as_ref().unwrap().keyboard);
    assert!(rt.state.fitted[&id(1)].0.1 > before.0.1);
    assert_eq!(rt.state.fitted.len(), 1);
    let sizes: Vec<_> = outbound(&client)
        .into_iter()
        .filter_map(|f| match f {
            FrameKind::ResizeTerminal { terminal_id, .. } => Some(terminal_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        sizes,
        [id(1)],
        "keyboard destination, not every spring frame"
    );
    let (x, y) = rt.find("phone-navigation").unwrap();
    touch(&mut rt, Started, x, y);
    for dx in [-20.0, -40.0, -60.0, -80.0] {
        touch(&mut rt, Moved, x + dx, y);
    }
    touch(&mut rt, Ended, x - 80.0, y);
    settle(&mut rt);
    let sizes: Vec<_> = outbound(&client)
        .into_iter()
        .filter_map(|f| match f {
            FrameKind::ResizeTerminal { terminal_id, .. } => Some(terminal_id),
            _ => None,
        })
        .collect();
    assert_eq!(sizes, [id(3)], "new focus gets one destination size");
    assert_eq!(
        rt.state.areas.len(),
        2,
        "hidden siblings never laid out for sizing"
    );
}

#[test]
fn document_closure_ratio_changes_groups_and_unarranged_resources_reconcile() {
    use phux_client_core::layout::leaves;
    let (client, mut rt) = fixture(true);
    rt.update(|d, _| d.focus_pane(&id(4)));
    settle(&mut rt);
    remove_document(&client, 4);
    poll(&mut rt);
    settle(&mut rt);
    assert_eq!(
        focus(&rt),
        id(3),
        "closed document uses full-group neighbor"
    );
    assert_eq!(
        leaves(rt.state.phone.as_ref().unwrap().tree.as_ref().unwrap()),
        [id(1), id(2), id(3)]
    );
    let (x, y) = rt.find("phone-navigation").unwrap();
    touch(&mut rt, Started, x, y);
    touch(&mut rt, Moved, x + 60.0, y);
    let mut changed = doc();
    let LayoutNode::Split { ratio, .. } = changed.groups[0].state.tree.as_mut().unwrap() else {
        panic!("split")
    };
    *ratio = 0.6;
    install(&client, &changed, 2);
    poll(&mut rt);
    touch(&mut rt, Ended, x + 60.0, y);
    assert_eq!(
        focus(&rt),
        id(3),
        "layout change cancels in-flight navigation"
    );
    assert_eq!(rt.state.phone.as_ref().unwrap().travel, [0.0; 2]);
    tap(&mut rt, "phone-tab-1");
    poll(&mut rt);
    settle(&mut rt);
    assert_eq!(focus(&rt), id(5));
    assert!(
        rt.find("phone-minimap").is_none(),
        "single-pane group has no map"
    );
    assert!(rt.state.others.is_empty(), "no session-wide grid");
    rt.update(|d, _| d.want(id(6)));
    poll(&mut rt);
    settle(&mut rt);
    assert_eq!(focus(&rt), id(6));
    assert_eq!(
        leaves(rt.state.phone.as_ref().unwrap().tree.as_ref().unwrap()),
        [id(6)]
    );
    assert!(rt.state.others.is_empty());
    assert!(rt.find("phone-minimap").is_none());
    no_input(&client);
}

#[test]
fn a_reconnect_reopens_all_retained_views_and_forgets_the_old_gesture() {
    let (client, mut rt) = fixture(true);
    let before = rt.state.shown.as_ref().unwrap().view;
    let old_other = rt.state.others[&id(2)];
    let (x, y) = rt.find("phone-navigation").unwrap();
    touch(&mut rt, Started, x, y);
    touch(&mut rt, Moved, x - 70.0, y);
    rt.update(|d, _| {
        assert!(d.reopen_after_reconnect(&client));
    });
    poll(&mut rt);
    touch(&mut rt, Ended, x - 70.0, y);
    settle(&mut rt);
    assert_eq!(focus(&rt), id(1));
    assert_ne!(rt.state.shown.as_ref().unwrap().view, before);
    assert_ne!(rt.state.others[&id(2)], old_other);
    assert_eq!(rt.state.others.len(), 3);
    assert!(!rt.state.layout_is_stale());
    no_input(&client);
}

#[test]
fn desktop_keeps_splits_zoom_divider_preview_and_all_pane_sizing() {
    use phux_client_core::layout::leaves;
    let (client, mut rt) = fixture(false);
    assert_eq!(
        leaves(rt.state.layout.as_ref().unwrap()),
        [id(1), id(2), id(3), id(4)]
    );
    assert_eq!(rt.state.on_screen().len(), 4);
    assert!(rt.state.fitted.contains_key(&id(2)));
    assert!(rt.state.fitted.contains_key(&id(3)));
    assert!(rt.find("phone-canvas").is_none());
    assert!(rt.find("divider-[]").is_some());
    rt.update(|d, _| d.drag_divider(vec![], 0.6, false));
    assert!(
        matches!(rt.state.layout.as_ref().unwrap(), LayoutNode::Split { ratio, .. } if *ratio == 0.6)
    );
    rt.update(|d, _| {
        d.dragging = None;
        d.toggle_zoom();
    });
    assert_eq!(leaves(rt.state.layout.as_ref().unwrap()), [id(1)]);
    rt.update(|d, _| d.toggle_zoom());
    assert_eq!(leaves(rt.state.layout.as_ref().unwrap()).len(), 4);
    no_input(&client);
}

#[test]
fn identity_changes_cancel_navigation_even_with_identical_resource_rectangles() {
    let (client, mut rt) = fixture(true);
    let (x, y) = rt.find("phone-navigation").unwrap();
    touch(&mut rt, Started, x, y);
    touch(&mut rt, Moved, x - 65.0, y);
    let mut renamed_context = doc();
    renamed_context.groups[0].id = [9; 16];
    install(&client, &renamed_context, 2);
    poll(&mut rt);
    touch(&mut rt, Ended, x - 65.0, y);
    assert_eq!(focus(&rt), id(1));
    assert_eq!(rt.state.phone.as_ref().unwrap().travel, [0.0; 2]);
    no_input(&client);
}

#[test]
fn dead_views_are_recreated_and_closed_terminal_focus_uses_full_group() {
    use phux_protocol::wire::frame::{CloseReason, FrameKind};
    let (client, mut rt) = fixture(true);
    let dead = rt.state.others[&id(2)].unwrap();
    client.destroy_view(dead).unwrap();
    poll(&mut rt);
    assert_ne!(rt.state.others[&id(2)], Some(dead));
    rt.update(|d, _| d.focus_pane(&id(3)));
    settle(&mut rt);
    client
        .feed(FrameKind::ResourceClosed {
            terminal_id: id(3),
            exit_status: Some(0),
            signal: None,
            reason: CloseReason::Unknown,
        })
        .unwrap();
    poll(&mut rt);
    settle(&mut rt);
    assert_eq!(focus(&rt), id(2));
    assert!(!rt.state.others.contains_key(&id(3)));
    assert!(!rt.state.layout_is_stale());
    no_input(&client);
}
