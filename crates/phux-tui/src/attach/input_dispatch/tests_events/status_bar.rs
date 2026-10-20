// ---- status-bar window tabs ------------------------------------------------------

/// A painter with the `windows` widget, fed "0:bash 1:vim" (window 0 on
/// columns 0..=5, the separator on 6, window 1 on 7..=11) and painted once
/// so its hit-test cache is populated.
fn painted_windows_bar(position: Position) -> StatusBarPainter {
    use phux_config::widget::{StatusBar, WidgetRegistry, WindowInfo};
    let cfg = phux_config::StatusCfg {
        left: vec![phux_config::Widget::Bare("windows".into())],
        ..Default::default()
    };
    let bar = StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).expect("bar builds");
    let mut painter = StatusBarPainter::new(bar, position);
    let tab = |name: &str, active| WindowInfo {
        name: name.to_owned(),
        active,
        zoomed: false,
        attention: false,
        branch: None,
        exited: None,
        badge: None,
    };
    painter.set_windows(vec![tab("bash", true), tab("vim", false)]);
    paint(&mut painter, 80);
    painter
}

fn paint(painter: &mut StatusBarPainter, cols: u16) {
    painter
        .paint(
            &mut Vec::new(),
            BarInset::NONE,
            cols,
            24,
            &make_context("", std::time::SystemTime::UNIX_EPOCH),
        )
        .expect("paint");
}

/// A two-window workspace with a painted bar at `position` (or a reserved
/// row with no painter).
fn bar_env(position: Position, with_painter: bool) -> Env<'static> {
    let mut fx = CtxFixture::default();
    fx.workspace.add_group("two".to_owned(), tid(2));
    fx.workspace.select(0);
    fx.bar = Some(position);
    fx.status_bar = with_painter.then(|| painted_windows_bar(position));
    Env::new(fx)
}

/// Bar-row clicks: a left press on window 1's tab selects it (bottom or top
/// dock); a right press also opens its menu, and off the tabs opens the
/// session menu; a non-tab cell or a missing painter is consumed. Nothing
/// reaches a pane.
#[tokio::test]
async fn bar_clicks_select_tabs_and_never_reach_a_pane() {
    let cases = [
        (
            Position::Bottom,
            true,
            left(MouseAction::Press, 8, 23),
            1,
            false,
        ),
        (
            Position::Top,
            true,
            left(MouseAction::Press, 8, 0),
            1,
            false,
        ),
        (
            Position::Bottom,
            true,
            left(MouseAction::Press, 6, 23),
            0,
            false,
        ),
        (
            Position::Bottom,
            true,
            left(MouseAction::Press, 40, 23),
            0,
            false,
        ),
        (Position::Bottom, true, right_press(8, 23), 1, true),
        (Position::Bottom, true, right_press(40, 23), 0, true),
        (
            Position::Bottom,
            false,
            left(MouseAction::Press, 8, 23),
            0,
            false,
        ),
    ];
    for (position, painter, event, active, overlay) in cases {
        let mut env = bar_env(position, painter);
        let sent = env.dispatch(vec![event.clone()]).await;
        assert_eq!(env.fx.workspace.active, active, "{event:?}");
        assert_eq!(env.fx.overlays.is_active(), overlay, "{event:?}");
        assert!(sent.frames.is_empty(), "{event:?}: {:?}", sent.frames);
    }
}

/// With a TOP bar, the bottom row is pane content: the click forwards.
#[tokio::test]
async fn bar_claim_leaves_pane_content_alone() {
    let mut env = bar_env(Position::Top, true);
    let sent = env.dispatch(vec![left(MouseAction::Press, 8, 23)]).await;
    assert_eq!(env.fx.workspace.active, 0);
    assert!(
        matches!(sent.frames.as_slice(), [FrameKind::InputMouse { terminal_id, .. }] if *terminal_id == tid(1)),
        "{:?}",
        sent.frames
    );
}

/// A tab dragged onto another tab moves into its slot (still active); off
/// the bar or back on itself it stays. Motion marks the slot; release or
/// focus loss clears it. Nothing reaches a pane.
#[tokio::test]
async fn dragging_a_tab_reorders_windows() {
    let grab = left(MouseAction::Press, 2, 23);
    let over = left(MouseAction::Motion, 9, 23);
    let cases = [
        (
            vec![
                grab.clone(),
                left(MouseAction::Motion, 5, 23),
                over.clone(),
                left(MouseAction::Release, 9, 23),
            ],
            ["two", "1"],
            None,
        ),
        (
            vec![grab.clone(), left(MouseAction::Release, 9, 10)],
            ["1", "two"],
            None,
        ),
        (
            vec![grab.clone(), left(MouseAction::Release, 3, 23)],
            ["1", "two"],
            None,
        ),
        (
            vec![grab.clone(), left(MouseAction::Release, 40, 23)],
            ["1", "two"],
            None,
        ),
        (vec![grab.clone(), over.clone()], ["1", "two"], Some(1)),
        (
            vec![grab, over, InputEvent::Focus(FocusEvent::Lost)],
            ["1", "two"],
            None,
        ),
    ];
    for (events, order, marker) in cases {
        let mut env = bar_env(Position::Bottom, true);
        let sent = env.dispatch(events).await;
        assert_eq!(window_names(&env), order);
        assert_eq!(drop_at(&env), marker);
        assert!(
            !sent
                .frames
                .iter()
                .any(|f| matches!(f, FrameKind::InputMouse { .. })),
            "{:?}",
            sent.frames
        );
        if order == ["two", "1"] {
            assert_eq!(env.fx.workspace.active, 1);
        }
    }
}

/// A tab column commits a dispatched `select-window`; non-tab columns and a
/// missing painter commit nothing; every painted navigation hint dispatches.
#[test]
fn bar_click_action_maps_tabs_and_hints_to_dispatched_actions() {
    use phux_config::widget::{CellHit, StatusBar, WidgetRegistry};
    let painter = painted_windows_bar(Position::Bottom);
    let resolved = bar_click_action(Some(&painter), 8).expect("tab column hits");
    assert_eq!(
        (resolved.action.as_str(), index_arg(&resolved)),
        ("select-window", Some(1))
    );
    assert!(ACTION_NAMES.contains(&resolved.action.as_str()));
    assert!(
        bar_click_action(Some(&painter), 6).is_none()
            && bar_click_action(Some(&painter), 40).is_none()
    );
    assert!(bar_click_action(None, 8).is_none());

    let cfg = phux_config::StatusCfg {
        center: vec![phux_config::Widget::Bare("help-hints".into())],
        ..Default::default()
    };
    let bar = StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).expect("bar builds");
    let mut painter = StatusBarPainter::new(bar, Position::Bottom);
    paint(&mut painter, 100);
    let mut actions = std::collections::BTreeSet::new();
    for x in 0..100 {
        if matches!(painter.hit_at(x), Some(CellHit::Action(_))) {
            let resolved = bar_click_action(Some(&painter), x).expect("painted action dispatches");
            assert!(resolved.args.is_empty());
            actions.insert(resolved.action);
        }
    }
    let expected = [
        "command-palette",
        "copy-mode",
        "session-picker",
        "settings",
        "show-help",
    ];
    assert_eq!(actions, expected.into_iter().map(str::to_owned).collect());
}
