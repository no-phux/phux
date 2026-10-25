use super::*;

fn setup(desk: &Entity<Desk>, cx: &mut VisualTestContext) {
    desk.update(cx, |desk, cx| {
        let workspace = &mut desk.link.as_mut().unwrap().workspace;
        workspace.tabs = tabs(3);
        workspace.active_id = "fixture-a".into();
        cx.notify();
    });
    draw(cx);
}

fn assert_top_alignment(desk: &Entity<Desk>, cx: &mut VisualTestContext) {
    for id in ids(desk, cx) {
        let tab_selector = Box::leak(format!("tab-{id}").into_boxed_str());
        let preview_selector = Box::leak(format!("tab-peek-{id}").into_boxed_str());
        let tab = cx.debug_bounds(tab_selector).unwrap();
        let preview = cx.debug_bounds(preview_selector).unwrap();
        assert_eq!(preview.left(), tab.left(), "{id}: leading edge");
        assert_eq!(preview.right(), tab.right(), "{id}: trailing edge");
    }
}

#[gpui::test]
fn top_previews_follow_tab_edges_across_sizes_skins_and_sidebar_changes(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    setup(&desk, cx);
    desk.update(cx, |desk, cx| {
        desk.link.as_mut().unwrap().workspace.tabs = tabs(6);
        cx.notify();
    });
    run(&desk, "tab-peek", cx);
    for skin in ["glass", "classic", "terminal"] {
        for sidebar in [false, true] {
            desk.update(cx, |desk, cx| {
                desk.update_prefs(
                    |prefs| {
                        prefs.skin_id = skin.into();
                        prefs.sidebar_visible = sidebar;
                    },
                    cx,
                );
            });
            for width in [800., 1280., 2000.] {
                cx.simulate_resize(gpui::size(px(width), px(600.)));
                draw(cx);
                assert_top_alignment(&desk, cx);
            }
        }
    }
}

#[gpui::test]
fn top_previews_and_tabs_share_wheel_and_keyboard_scrolling(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(800.), px(600.)));
    setup(&desk, cx);
    desk.update(cx, |desk, cx| {
        desk.link.as_mut().unwrap().workspace.tabs = tabs(12);
        cx.notify();
    });
    run(&desk, "tab-peek", cx);
    let before = cx.debug_bounds("tab-fixture-a").unwrap();
    let preview = cx.debug_bounds("tab-peek-scroll").unwrap();
    let strip = cx.debug_bounds("top-tabs").unwrap();
    for position in [point(strip.center().x, preview.center().y), strip.center()] {
        cx.simulate_event(gpui::ScrollWheelEvent {
            position,
            delta: gpui::ScrollDelta::Pixels(point(px(-220.), px(0.))),
            ..Default::default()
        });
        draw(cx);
        assert_top_alignment(&desk, cx);
    }
    assert!(cx.debug_bounds("tab-fixture-a").unwrap().left() < before.left());
    for _ in 0..11 {
        cx.simulate_keystrokes("right");
        draw(cx);
        assert_top_alignment(&desk, cx);
    }
    let selected = cx.debug_bounds("tab-peek-fixture-l").unwrap();
    assert!(strip.left() <= selected.left() && selected.right() <= strip.right());
    // The visible card retains its hit target after following a scrolled tab.
    cx.simulate_click(selected.center(), Modifiers::none());
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert!(!desk.tab_peek.open);
        assert_eq!(desk.active_tab_id().as_deref(), Some("fixture-l"));
    });
}

#[gpui::test]
fn constrained_overview_keeps_last_preview_reachable_and_has_a_close_control(
    cx: &mut TestAppContext,
) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1000.), px(600.)));
    setup(&desk, cx);
    desk.update(cx, |desk, cx| {
        desk.changes.open = true;
        desk.link.as_mut().unwrap().workspace.tabs = tabs(6);
        cx.notify();
    });
    run(&desk, "tab-peek", cx);
    for _ in 0..5 {
        cx.simulate_keystrokes("right");
        draw(cx);
    }
    let shelf = cx.debug_bounds("tab-peek-scroll").unwrap();
    assert!(shelf.contains(&cx.debug_bounds("tab-peek-fixture-f").unwrap().center()));
    // Closing through the visible control is equivalent to Escape.
    let close = cx.debug_bounds("close-tab-peek").unwrap();
    cx.simulate_click(close.center(), Modifiers::none());
    draw(cx);
    assert!(cx.debug_bounds("tab-peek").is_none());
}

#[gpui::test]
fn peeking_translates_without_resizing_in_every_tab_position(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1000.), px(600.)));
    setup(&desk, cx);
    for position in ["tabs-top", "tabs-bottom", "tabs-side"] {
        run(&desk, position, cx);
        let before = cx.debug_bounds("workspace-body").unwrap();
        let tab = cx.debug_bounds("tab-fixture-a").unwrap();
        cx.simulate_click(tab.center(), Modifiers::none());
        draw(cx);
        let shelf = cx
            .debug_bounds("tab-peek")
            .expect("active tab click opens peek");
        let after = cx.debug_bounds("workspace-body").unwrap();
        assert_eq!(after.size, before.size, "peek must not shrink the terminal");
        assert_eq!(after.origin.x, before.origin.x);
        assert_eq!(after.origin.y - before.origin.y, shelf.size.height);
        cx.simulate_keystrokes("escape");
        draw(cx);
        assert!(cx.debug_bounds("tab-peek").is_none());
        assert_eq!(cx.debug_bounds("workspace-body").unwrap(), before);
    }
}

#[gpui::test]
fn miniatures_preserve_the_split_shape_even_when_the_tab_is_zoomed(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    setup(&desk, cx);
    desk.update(cx, |desk, cx| {
        let tab = &mut desk.link.as_mut().unwrap().workspace.tabs[0];
        let mut second = layout::focused_placement(tab).unwrap().clone();
        second.id = "split-right".into();
        layout::split_at(&mut tab.root, "fixture-a", second, Axis::Row, false);
        if let LayoutNode::Split { ratio, .. } = &mut tab.root {
            *ratio = 0.3;
        }
        tab.zoomed_id = Some("fixture-a".into());
        cx.notify();
    });
    let before = desk.read_with(cx, |desk, _| desk.active_tab().unwrap().clone());
    run(&desk, "tab-peek", cx);
    let left = cx.debug_bounds("tab-peek-pane-fixture-a").unwrap();
    let right = cx.debug_bounds("tab-peek-pane-split-right").unwrap();
    assert!(left.size.width < right.size.width);
    assert_eq!(left.size.height, right.size.height);
    assert_eq!(right.left() - left.right(), px(2.));
    assert!(left.size.height > px(0.));
    run(&desk, "tab-peek", cx);
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.active_tab().unwrap(), &before)
    });
}

#[gpui::test]
fn previews_navigate_and_arrows_keep_the_shelf_open(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    setup(&desk, cx);
    cx.simulate_keystrokes("cmd-shift-\\");
    draw(cx);
    assert!(cx.debug_bounds("tab-peek").is_some());
    cx.simulate_keystrokes("right");
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert!(desk.tab_peek.open);
        assert_eq!(desk.active_tab_id().as_deref(), Some("fixture-b"));
    });
    let preview = cx.debug_bounds("tab-peek-fixture-c").unwrap();
    cx.simulate_click(preview.center(), Modifiers::none());
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert!(!desk.tab_peek.open);
        assert_eq!(desk.active_tab_id().as_deref(), Some("fixture-c"));
    });
    run(&desk, "tab-peek", cx);
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert!(cx.debug_bounds("tab-peek").is_none());
}

#[gpui::test]
fn peek_is_reachable_from_palette_and_keyboard_keeps_offscreen_tabs_visible(
    cx: &mut TestAppContext,
) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(800.), px(600.)));
    setup(&desk, cx);
    desk.update(cx, |desk, cx| {
        desk.link.as_mut().unwrap().workspace.tabs = tabs(10);
        cx.notify();
    });
    cx.simulate_keystrokes("cmd-shift-p");
    draw(cx);
    cx.simulate_keystrokes("p e e k space t a b s");
    draw(cx);
    assert!(cx.debug_bounds("palette-result-tab-peek").is_some());
    cx.simulate_keystrokes("enter");
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.modal, Modal::None);
        assert!(desk.tab_peek.open);
    });
    for _ in 0..9 {
        cx.simulate_keystrokes("right");
        draw(cx);
    }
    let shelf = cx.debug_bounds("tab-peek-scroll").unwrap();
    let selected = cx.debug_bounds("tab-peek-fixture-j").unwrap();
    assert!(
        shelf.contains(&selected.center()),
        "keyboard selection must scroll into view"
    );
    let displaced = cx.debug_bounds("workspace-body").unwrap();
    cx.simulate_click(
        point(displaced.center().x, displaced.top() + px(50.)),
        Modifiers::none(),
    );
    draw(cx);
    assert!(
        cx.debug_bounds("tab-peek").is_none(),
        "clicking the displaced work dismisses"
    );
}

#[gpui::test]
fn peek_takes_keys_from_changes_and_even_an_ignore_binding_finishes_dismissal(
    cx: &mut TestAppContext,
) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    setup(&desk, cx);
    cx.update(|window, cx| {
        desk.update(cx, |desk, cx| {
            desk.changes.open = true;
            desk.prepare_changes_input(window, cx);
            let input = desk.changes.message.as_ref().unwrap().0.clone();
            input.update(cx, |input, cx| {
                input.set_value("keep this draft", window, cx);
                input.focus(window, cx);
            });
            assert!(desk.changes.has_keys(window, cx));
            desk.execute("tab-peek", window, cx);
            assert!(
                desk.focus.is_focused(window),
                "peek must take focus from Changes"
            );
            assert!(!desk.changes.has_keys(window, cx));
        })
    });
    draw(cx);
    cx.simulate_keystrokes("right x");
    draw(cx);
    desk.read_with(cx, |desk, cx| {
        assert_eq!(desk.active_tab_id().as_deref(), Some("fixture-b"));
        assert_eq!(
            desk.changes
                .message
                .as_ref()
                .unwrap()
                .0
                .read(cx)
                .value()
                .as_str(),
            "keep this draft"
        );
    });
    // No helper draw/cx.notify after dispatch: the command itself must mark
    // the desk dirty even though an ignore action has no side effects.
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.execute("ignore", window, cx)));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("tab-peek").is_none());
}

#[gpui::test]
fn tab_selection_drag_modifiers_and_rename_do_not_accidentally_peek(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    setup(&desk, cx);
    let b = cx.debug_bounds("tab-fixture-b").unwrap();
    cx.simulate_click(b.center(), Modifiers::none());
    draw(cx);
    assert!(
        cx.debug_bounds("tab-peek").is_none(),
        "inactive click only selects"
    );
    let b = cx.debug_bounds("tab-fixture-b").unwrap();
    cx.simulate_mouse_down(b.center(), MouseButton::Left, Modifiers::none());
    draw(cx);
    let target = point(b.center().x + px(12.), b.center().y);
    cx.simulate_mouse_move(target, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::none());
    draw(cx);
    assert!(
        cx.debug_bounds("tab-peek").is_none(),
        "a small drag is not a click"
    );
    cx.simulate_click(
        b.center(),
        Modifiers {
            shift: true,
            ..Modifiers::none()
        },
    );
    draw(cx);
    assert!(cx.debug_bounds("tab-peek").is_none());
    cx.simulate_event(gpui::MouseDownEvent {
        position: b.center(),
        button: MouseButton::Left,
        click_count: 2,
        modifiers: Modifiers::none(),
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseUpEvent {
        position: b.center(),
        button: MouseButton::Left,
        click_count: 2,
        modifiers: Modifiers::none(),
    });
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert!(matches!(desk.modal, Modal::Rename { .. }));
        assert!(!desk.tab_peek.open);
    });
}

#[gpui::test]
fn live_peek_never_sends_resize_input_or_attach_and_releases_scenes_on_close(
    cx: &mut TestAppContext,
) {
    use phux_protocol::{
        ResourceId,
        ids::{BootstrapId, StreamId},
        wire::frame::FrameKind,
    };
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let mut connection = crate::workspace::view_tests::attached();
    let workspace = crate::workspace::view_tests::opened(&mut connection);
    let client = connection.client.clone();
    desk.update(cx, |desk, cx| {
        desk.link = Some(Link {
            connection,
            workspace,
        });
        cx.notify();
    });
    let cx = &mut VisualTestContext::from_window(handle, cx);
    draw(cx);
    let before = cx.debug_bounds("workspace-body").unwrap();
    let placement = desk.read_with(cx, |desk, _| desk.focused_placement().unwrap().clone());
    let frame = client.acquire_view(placement.view_id.unwrap()).unwrap();
    let _ = client.take_outbound();
    run(&desk, "tab-peek", cx);
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.tab_peek.scenes.len(), 1);
        assert!(!desk.terminal_settings(&placement, true).unwrap().focused);
    });
    client
        .feed(FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(7),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq: 1,
            bytes: b"\r\nLIVE PEEK".to_vec().into(),
        })
        .unwrap();
    // The actual activity drain must invalidate the shelf for paint-only batches.
    desk.update(cx, |desk, cx| desk.on_activity(cx));
    draw(cx);
    let live = client.acquire_view(placement.view_id.unwrap()).unwrap();
    assert!(live.generation > frame.generation);
    assert_eq!((live.cols, live.rows), (frame.cols, frame.rows));
    cx.simulate_keystrokes("x escape");
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert!(!desk.tab_peek.open);
        assert!(desk.tab_peek.scenes.is_empty());
    });
    assert_eq!(cx.debug_bounds("workspace-body").unwrap(), before);
    for bytes in client.take_outbound() {
        let frame = FrameKind::decode(&bytes).unwrap().0;
        match frame {
            FrameKind::FrameAck { .. } | FrameKind::InputFocus { .. } => {}
            FrameKind::Command {
                command: phux_protocol::wire::frame::Command::ApplyInput { events, .. },
                ..
            } => {
                assert!(
                    events
                        .iter()
                        .all(|event| matches!(event, phux_protocol::input::InputEvent::Focus(_))),
                    "{events:?}"
                );
            }
            other => panic!("peek must not attach, resize or send terminal input: {other:?}"),
        }
    }
}
