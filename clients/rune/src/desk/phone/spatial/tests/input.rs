use super::*;

#[test]
fn a_collapsed_focus_can_be_picked_without_nonfinite_runtime_geometry() {
    let (client, mut rt) = fixture(true);
    let mut collapsed = doc();
    let LayoutNode::Split { ratio, .. } = collapsed.groups[0].state.tree.as_mut().unwrap() else {
        panic!("split")
    };
    *ratio = 0.0;
    // The wire adapter clamps ratios before publication. Inject this local
    // boundary case only after the real refresh created all sibling views.
    rt.update(|d, _| {
        let phone = d.phone.as_mut().unwrap();
        phone.reconcile(collapsed.groups[0].state.tree.clone(), phone.context);
    });
    settle(&mut rt);
    assert_eq!(focus(&rt), id(1));
    assert!(
        geometry(&rt, "phone-canvas")
            .rect
            .iter()
            .all(|v| v.is_finite())
    );
    tap(&mut rt, "phone-pane-picker");
    settle(&mut rt);
    tap(&mut rt, &format!("phone-pick-{}", id(3)));
    settle(&mut rt);
    assert_eq!(focus(&rt), id(3));
    tap(&mut rt, "phone-pane-picker");
    settle(&mut rt);
    tap(&mut rt, &format!("phone-pick-{}", id(1)));
    settle(&mut rt);
    assert_eq!(focus(&rt), id(1));
    let pane = geometry(&rt, &format!("phone-spatial-pane-{}", id(1)));
    let stage = geometry(&rt, "phone-spatial-stage");
    assert_eq!(pane.rect, stage.rect);
    assert!(
        rt.find(&format!("phone-spatial-pane-{}", id(3))).is_none(),
        "opaque siblings must not cover the collapsed focus"
    );
    assert_eq!(
        rt.state.others.len(),
        3,
        "fallback retains every group view"
    );
    no_input(&client);
}
use phux_protocol::wire::frame::FrameKind;
use rune::{Key, KeyEvent, Mods};

fn key(rt: &mut rune::Runtime<Desk>, key: Key) {
    rt.key(KeyEvent {
        key,
        mods: Mods::default(),
    });
    rt.frame(1.0 / 60.0);
}

#[test]
fn picker_is_compact_named_and_traps_physical_soft_and_paste_input() {
    let (client, mut rt) = fixture(true);
    let map = geometry(&rt, "phone-minimap");
    assert_eq!([map.rect[2], map.rect[3]], [96.0, 64.0]);
    tap(&mut rt, "phone-pane-picker");
    settle(&mut rt);
    for (n, name) in [(1, "Build"), (2, "Tests"), (3, "Logs")] {
        assert!(
            geometry(&rt, &format!("phone-pick-{}", id(n)))
                .label
                .unwrap()
                .starts_with(name)
        );
    }
    key(&mut rt, Key::Text("sentinel".into()));
    rt.paste("paste sentinel");
    rt.frame(1.0 / 60.0);
    tap(&mut rt, "phone-cap-120");
    rt.update(|d, tx| {
        d.ime(&rune::Ime::Preedit("composing".into(), None));
        d.ime(&rune::Ime::Commit("IME sentinel".into()));
        d.key(
            &KeyEvent {
                key: Key::Text("direct key sentinel".into()),
                mods: Mods::default(),
            },
            tx,
        );
    });
    assert!(rt.state.phone.as_ref().unwrap().picker);
    assert!(rt.state.phone_picker_visible());
    assert!(rt.state.modal().unwrap().takes_pointer());
    assert_eq!(focus(&rt), id(1));
    no_input(&client);
    key(&mut rt, Key::Tab);
    key(&mut rt, Key::Enter);
    assert_eq!(
        focus(&rt),
        id(1),
        "Tab/Return activates the left boundary action"
    );
    assert!(rt.state.phone.as_ref().unwrap().picker);
    key(&mut rt, Key::Right);
    assert_eq!(focus(&rt), id(3));
    key(&mut rt, Key::Escape);
    assert!(!rt.state.phone.as_ref().unwrap().picker);
    settle(&mut rt);
    no_input(&client);
    key(&mut rt, Key::Tab);
    assert!(
        outbound(&client)
            .iter()
            .any(|f| matches!(f, FrameKind::InputKey { event, .. }
        if event.key == phux_protocol::input::key::PhysicalKey::Tab)),
        "normal chrome must not steal terminal Tab"
    );
}

#[test]
fn spatial_find_retains_its_editor_and_never_sends_soft_keys_behind_it() {
    let (client, mut rt) = fixture(true);
    rt.update(|d, _| d.open_find());
    settle(&mut rt);
    tap(&mut rt, "phone-cap-116");
    tap(&mut rt, "phone-cap-104");
    assert!(rt.state.find_query.focused);
    assert_eq!(rt.state.find_query.text, "th");
    let before = rt.state.generation;
    tap(&mut rt, "phone-cap-return");
    assert!(rt.state.generation > before);
    tap(&mut rt, "phone-key-Escape");
    assert!(rt.state.find.is_none());
    no_input(&client);
}

#[test]
fn confirmation_inside_spatial_find_keeps_the_query_until_confirmation_closes() {
    let (client, mut rt) = fixture(true);
    rt.update(|d, _| {
        d.open_find();
        d.pending = Some(crate::desk::Pending::Close(id(1)));
    });
    settle(&mut rt);
    tap(&mut rt, "phone-cap-116");
    assert!(rt.state.find_query.text.is_empty());
    tap(&mut rt, "phone-key-Escape");
    assert!(rt.state.pending.is_none());
    assert!(rt.state.find.is_some());
    tap(&mut rt, "phone-cap-104");
    assert!(rt.state.find_query.focused);
    assert_eq!(rt.state.find_query.text, "h");
    no_input(&client);
}

#[test]
fn disabled_navigation_surfaces_do_not_pass_mouse_presses_into_programs() {
    let (client, mut rt) = fixture(true);
    output(&client, 1, 1, b"\x1b[?1002h\x1b[?1006h");
    poll(&mut rt);
    rt.update(|d, _| d.open_find());
    settle(&mut rt);
    for key in ["phone-minimap", "phone-navigation", "phone-pane-picker"] {
        tap(&mut rt, key);
        assert_eq!(focus(&rt), id(1));
    }
    assert!(rt.state.find.is_some());
    no_input(&client);
}

#[test]
fn accessible_picker_actions_focus_named_panes_without_terminal_input() {
    use rune::accesskit::Action;
    let (client, mut rt) = fixture(true);
    assert!(rt.accessibility_action("phone-pane-picker", Action::Click, None));
    settle(&mut rt);
    assert!(rt.state.phone.as_ref().unwrap().picker);
    assert!(rt.accessibility_action("phone-direction-Down", Action::Click, None));
    settle(&mut rt);
    assert_eq!(focus(&rt), id(2));
    assert!(rt.accessibility_action(&format!("phone-pick-{}", id(3)), Action::Click, None));
    settle(&mut rt);
    assert_eq!(focus(&rt), id(3));
    assert!(!rt.state.phone.as_ref().unwrap().picker);
    no_input(&client);
}

#[test]
fn actual_map_cancel_slop_boundary_and_second_finger_send_no_input() {
    let (client, mut rt) = fixture(true);
    let [mx, my, w, h] = geometry(&rt, "phone-minimap").rect;
    let (x, y) = (mx + w * 0.1, my + h * 0.2);
    touch(&mut rt, Started, x, y);
    touch(&mut rt, Moved, x - 60.0, y);
    touch(&mut rt, Cancelled, x - 60.0, y);
    settle(&mut rt);
    assert_eq!(focus(&rt), id(1));
    for dx in [4.0, 55.0] {
        touch(&mut rt, Started, x, y);
        touch(&mut rt, Moved, x + dx, y);
        touch(&mut rt, Ended, x + dx, y);
        settle(&mut rt);
        assert_eq!(focus(&rt), id(1));
    }
    touch(&mut rt, Started, x, y);
    rt.touch(2, Started, mx + w * 0.8, my + h * 0.2, 2.0);
    rt.touch(2, Moved, x - 50.0, y, 2.1);
    rt.touch(2, Ended, x - 50.0, y, 2.2);
    touch(&mut rt, Ended, x, y);
    assert_eq!(focus(&rt), id(1));
    assert_eq!(rt.state.phone.as_ref().unwrap().travel, [0.0; 2]);
    no_input(&client);
}

#[test]
fn extreme_finite_focus_uses_the_same_full_stage_fallback_for_camera_and_pane() {
    let (client, mut rt) = fixture(true);
    // A legal local tree can place a positive-width focus far from zero;
    // Rune's f32 edge representation cannot preserve x + width at this scale.
    let tiny = split(
        SplitDir::Horizontal,
        0.5,
        split(
            SplitDir::Vertical,
            0.5,
            LayoutNode::Leaf(id(2)),
            LayoutNode::Leaf(id(4)),
        ),
        split(
            SplitDir::Horizontal,
            1e-10,
            LayoutNode::Leaf(id(1)),
            LayoutNode::Leaf(id(3)),
        ),
    );
    let literal = rects(&tiny)
        .into_iter()
        .find(|(id, _)| id == &ResourceId::local(1))
        .unwrap()
        .1;
    assert!(literal.has_area());
    assert!(camera_fallback(literal, [400.0, 400.0]));
    rt.update(|d, _| {
        let phone = d.phone.as_mut().unwrap();
        phone.reconcile(Some(tiny.clone()), phone.context);
    });
    settle(&mut rt);
    let stage = geometry(&rt, "phone-spatial-stage");
    let focused = geometry(&rt, &format!("phone-spatial-pane-{}", id(1)));
    assert_eq!(focused.rect, stage.rect);
    assert!(
        geometry(&rt, "phone-canvas")
            .rect
            .iter()
            .all(|v| v.is_finite())
    );
    assert!(rt.find(&format!("phone-spatial-pane-{}", id(3))).is_none());
    assert_eq!(rt.state.others.len(), 3);
    assert_eq!(rt.state.phone.as_ref().unwrap().tree.as_ref(), Some(&tiny));
    no_input(&client);
}
