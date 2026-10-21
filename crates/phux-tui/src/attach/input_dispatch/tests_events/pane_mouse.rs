// ---- pane mouse routing ------------------------------------------------------------

/// [`two_pane_workspace`] at 80x24 (no bar or sidebar), with a published
/// 39x24 replica per `(id, vt)` seed and `cell_px` cells.
fn two_pane_env(seed_vt: &[(ResourceId, &[u8])], cell_px: (u16, u16)) -> Env<'static> {
    let mut fx = CtxFixture::default();
    fx.workspace = two_pane_workspace();
    fx.cell_px = cell_px;
    let entries: Vec<_> = seed_vt.iter().map(|(id, vt)| (id, 39, 24, *vt)).collect();
    Env::new(fx).published(&entries)
}

/// The divider column, found by hit-testing rather than hardcoding rounding.
fn divider_x() -> u16 {
    use crate::multi_pane::{RouteDecision, route_mouse_event};
    let workspace = two_pane_workspace();
    let ls = workspace.active_group().expect("window");
    let content = content_rect((80, 24), None, None);
    (0..80u16)
        .find(|&x| {
            let probe = mev(MouseAction::Press, MouseButton::Left, f64::from(x), 5.0);
            matches!(
                route_mouse_event(ls, content, (80, 24), &probe),
                RouteDecision::Divider { .. }
            )
        })
        .expect("a two-pane split has a divider column")
}

/// A mouse event over pane 2 (column 70, row 5).
fn over_pane_2(action: MouseAction, button: MouseButton) -> InputEvent {
    mouse(action, button, 70.0, 5.0)
}

const TRACKING: &[u8] = b"\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";

/// ADR-0124: a pane retained after its process exited takes no input; its
/// live neighbour still does.
#[tokio::test]
async fn input_to_a_retained_pane_is_dropped_by_the_dispatcher() {
    for (exited, events, forwarded) in [
        (
            tid(1),
            vec![
                press(PhysicalKey::A, Some("a")),
                press(PhysicalKey::Enter, None),
            ],
            false,
        ),
        (tid(2), vec![press(PhysicalKey::A, Some("a"))], true),
    ] {
        let mut env = two_pane_env(&[(tid(1), b"final screen"), (tid(2), b"")], (8, 16));
        env.panes.get_mut(&exited).expect("slot").exited =
            Some(super::super::pane_state::ExitMark {
                status: Some(0),
                signal: None,
            });
        let sent = env.dispatch(events).await;
        if forwarded {
            assert!(
                matches!(sent.frames.as_slice(), [FrameKind::InputKey { terminal_id, .. }] if *terminal_id == tid(1)),
                "{:?}",
                sent.frames
            );
        } else {
            assert!(sent.frames.is_empty(), "{:?}", sent.frames);
        }
    }
}

/// A second press during a divider drag is consumed (no focus move, no
/// forward), and the release that follows still ends the drag.
#[tokio::test]
async fn a_press_during_a_divider_drag_is_consumed() {
    let grab = mouse(
        MouseAction::Press,
        MouseButton::Left,
        f64::from(divider_x()),
        5.0,
    );
    let mut env = two_pane_env(&[], (1, 1));
    let sent = env
        .dispatch(vec![
            grab.clone(),
            over_pane_2(MouseAction::Press, MouseButton::Left),
        ])
        .await;
    assert!(env.fx.drag.is_some());
    assert_eq!(env.focused, Some(tid(1)));
    assert!(sent.frames.is_empty(), "{:?}", sent.frames);

    let mut env = two_pane_env(&[], (1, 1));
    env.dispatch(vec![
        grab,
        over_pane_2(MouseAction::Press, MouseButton::Right),
        over_pane_2(MouseAction::Release, MouseButton::Left),
    ])
    .await;
    assert!(env.fx.drag.is_none());
}

/// `set-pane mouse off` is per pane: a press in an opted-out pane focuses it
/// but forwards nothing (nor opens a menu); an opted-in sibling still gets
/// its `INPUT_MOUSE` in pane-local coordinates.
#[tokio::test]
async fn mouse_opt_out_is_per_pane() {
    let mut env = two_pane_env(&[(tid(2), b"")], (1, 1));
    env.fx.mouse_optout = HashSet::from([tid(2)]);
    let sent = env
        .dispatch(vec![over_pane_2(MouseAction::Press, MouseButton::Left)])
        .await;
    assert_eq!(env.focused, Some(tid(2)));
    assert!(sent.frames.is_empty(), "{:?}", sent.frames);
    let sent = env
        .dispatch(vec![over_pane_2(MouseAction::Press, MouseButton::Right)])
        .await;
    assert!(sent.frames.is_empty() && !env.fx.overlays.is_active());

    let mut env = two_pane_env(&[], (1, 1));
    env.fx.mouse_optout = HashSet::from([tid(1)]);
    let sent = env
        .dispatch(vec![over_pane_2(MouseAction::Press, MouseButton::Left)])
        .await;
    assert_eq!(env.focused, Some(tid(2)));
    match sent.frames.as_slice() {
        [FrameKind::InputMouse { terminal_id, event }] => {
            assert_eq!(*terminal_id, tid(2));
            assert!(
                event.x < f64::from(divider_x()),
                "pane-local x: {}",
                event.x
            );
        }
        other => panic!("expected one INPUT_MOUSE, got {other:?}"),
    }
}

fn primary_scrollback_vt(lines: usize) -> Vec<u8> {
    (0..lines)
        .flat_map(|i| format!("line-{i:03}\r\n").into_bytes())
        .collect()
}

/// Wheel routing by the pane's screen and mode state: the alt screen (even
/// with prior primary history) synthesizes 3 arrows per notch unless the app
/// turned alternate scroll off or tracks the mouse (then it forwards); the
/// primary screen scrolls locally, forwarding when there is nothing to
/// scroll (no history, or already at the live tail).
#[tokio::test]
async fn wheel_routing_follows_the_panes_screen_and_modes() {
    enum Want {
        Arrows(PhysicalKey),
        Forward(MouseButton),
        Nothing,
    }
    let history = primary_scrollback_vt(40);
    let mut alt_with_history = history.clone();
    alt_with_history.extend_from_slice(b"\x1b[?1049h");
    let (up, down) = (MouseButton::Four, MouseButton::Five);
    let cases: [(&[u8], MouseButton, Want); 7] = [
        (b"\x1b[?1049h", up, Want::Arrows(PhysicalKey::ArrowUp)),
        (b"\x1b[?1049h", down, Want::Arrows(PhysicalKey::ArrowDown)),
        (&alt_with_history, up, Want::Arrows(PhysicalKey::ArrowUp)),
        (b"\x1b[?1049h\x1b[?1007l", up, Want::Forward(up)),
        (TRACKING, up, Want::Forward(up)),
        (&history, up, Want::Nothing),
        (&history, down, Want::Forward(down)),
    ];
    for (vt, button, want) in cases {
        let mut env = two_pane_env(&[(tid(2), vt)], (1, 1));
        let sent = env
            .dispatch(vec![over_pane_2(MouseAction::Press, button)])
            .await;
        match want {
            Want::Arrows(key) => {
                assert_eq!(sent.frames.len(), 3, "{:?}", sent.frames);
                for frame in &sent.frames {
                    assert!(
                        matches!(frame, FrameKind::InputKey { terminal_id, event } if *terminal_id == tid(2) && event.key == key),
                        "{frame:?}"
                    );
                }
            }
            Want::Forward(b) => assert!(
                matches!(sent.frames.as_slice(), [FrameKind::InputMouse { terminal_id, event }] if *terminal_id == tid(2) && event.button == b),
                "{:?}",
                sent.frames
            ),
            Want::Nothing => assert!(sent.frames.is_empty(), "{:?}", sent.frames),
        }
    }
    // An empty history forwards too rather than eating a no-op scroll.
    let mut env = two_pane_env(&[(tid(2), b"")], (1, 1));
    let sent = env
        .dispatch(vec![over_pane_2(MouseAction::Press, up)])
        .await;
    assert!(
        matches!(sent.frames.as_slice(), [FrameKind::InputMouse { .. }]),
        "{:?}",
        sent.frames
    );
}

/// Ctrl-left drag over a mouse-tracking TUI is the host-selection escape
/// hatch: it enters copy mode instead of reaching the app.
#[tokio::test]
async fn ctrl_left_press_in_mouse_tracking_pane_starts_host_copy() {
    let mut event = mev(MouseAction::Press, MouseButton::Left, 70.0, 5.0);
    event.mods = ModSet::CTRL;
    let mut env = two_pane_env(&[(tid(2), TRACKING)], (1, 1));
    let sent = env.dispatch(vec![InputEvent::Mouse(event)]).await;
    assert_eq!(env.focused, Some(tid(2)));
    assert!(sent.frames.is_empty() && env.fx.overlays.is_active());
}

/// ADR-0058: a right press over a pane that has not asked for the mouse
/// focuses it and opens the hover-tracking pane menu; a mouse-tracking app
/// owns the button, so it forwards instead. Clicking away closes the menu
/// and schedules the repaint that erases it.
#[tokio::test]
async fn right_press_opens_the_pane_menu_unless_the_app_owns_the_mouse() {
    let mut env = two_pane_env(&[(tid(2), b"")], (1, 1));
    let sent = env
        .dispatch(vec![over_pane_2(MouseAction::Press, MouseButton::Right)])
        .await;
    assert_eq!(env.focused, Some(tid(2)));
    assert!(env.fx.overlays.is_active() && env.fx.overlays.wants_pointer_hover());
    assert!(sent.frames.is_empty(), "{:?}", sent.frames);
    let sent = env
        .dispatch(vec![mouse(MouseAction::Press, MouseButton::Left, 2.0, 2.0)])
        .await;
    assert!(!env.fx.overlays.is_active() && sent.repainted);

    let mut env = two_pane_env(&[(tid(2), b"\x1b[?1000h\x1b[?1006h")], (1, 1));
    let sent = env
        .dispatch(vec![over_pane_2(MouseAction::Press, MouseButton::Right)])
        .await;
    assert!(!env.fx.overlays.is_active());
    assert!(
        matches!(sent.frames.as_slice(), [FrameKind::InputMouse { terminal_id, event }] if *terminal_id == tid(2) && event.button == MouseButton::Right),
        "{:?}",
        sent.frames
    );
}

/// SPEC input.md §3.1: forwarded positions are surface pixels, scaled from
/// pane-local cells at the send boundary.
#[tokio::test]
async fn forwarded_input_mouse_scales_cells_to_surface_pixels() {
    let mut env = two_pane_env(&[(tid(2), b"\x1b[?1000h\x1b[?1006h")], (8, 16));
    let sent = env
        .dispatch(vec![over_pane_2(MouseAction::Press, MouseButton::Left)])
        .await;
    // Pane 2 starts one column right of the divider and one row under the rail.
    let x = (70.0 - f64::from(divider_x()) - 1.0) * 8.0;
    let y = (5.0 - 1.0) * 16.0;
    match sent.frames.as_slice() {
        [FrameKind::InputMouse { event, .. }] => {
            assert!(
                (event.x - x).abs() < f64::EPSILON && (event.y - y).abs() < f64::EPSILON,
                "{event:?}"
            );
        }
        other => panic!("expected one INPUT_MOUSE, got {other:?}"),
    }
}
