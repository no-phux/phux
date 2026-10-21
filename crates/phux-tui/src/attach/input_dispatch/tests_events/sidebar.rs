// ---- sidebar hit targets -------------------------------------------------------

fn str_arg(r: &phux_config::keybind::ResolvedAction, key: &str) -> Option<String> {
    r.args.get(key)?.as_str().map(str::to_owned)
}

fn strip(h: u16) -> Rect {
    Rect {
        x: 0,
        y: 0,
        w: 28,
        h,
    }
}

/// Window blocks commit `select-window`, the footer `new-window`, the
/// collapse corner `toggle-sidebar`, and headers their management views;
/// blank rows and the separator commit nothing.
#[test]
fn sidebar_click_action_maps_rows_to_registry_actions() {
    // A 23-row strip: the fixed half Agents panel (rows 0-10), Sessions at
    // 11, its tabs at 14-15, footer at 22.
    let quiet = targets(0, 2, 1);
    let hit = |x, y| sidebar_click_action(strip(23), &quiet, x, y);
    let window = hit(4, 15).expect("window row");
    assert_eq!(
        (window.action.as_str(), index_arg(&window)),
        ("select-window", Some(1))
    );
    for (x, y, action) in [
        (4, 22, "new-window"),
        (27, 22, "toggle-sidebar"),
        (4, 11, "session-picker"),
    ] {
        let resolved = hit(x, y).expect(action);
        assert_eq!(resolved.action, action);
    }
    assert!(hit(4, 10).is_none() && hit(27, 0).is_none());
}

/// ADR-0148: a plugin section row focuses its pane; the section header is
/// inert.
#[test]
fn sidebar_plugin_rows_commit_focus_pane() {
    use crate::render::chrome::sidebar_sections::{PluginSection, PluginSectionRow, PluginShape};
    let section = PluginSection {
        title: "Builds".to_owned(),
        rows: 3,
        entries: vec![
            PluginSectionRow {
                text: "lint".to_owned(),
                window: 0,
                pane: 0,
            },
            PluginSectionRow {
                text: "test".to_owned(),
                window: 1,
                pane: 2,
            },
        ],
    };
    let mut t = targets(0, 1, 1);
    t.counts.plugin = PluginShape::of(std::slice::from_ref(&section));
    t.plugin = vec![vec![(0, 0), (1, 2)]];
    // The fixed half Agents panel tops the strip (rows 0-8), the 4-row
    // section at rows 9-12: header, two rows, one padding.
    let click = sidebar_click_action(strip(23), &t, 4, 11).expect("plugin row");
    assert_eq!(click.action, "focus-pane");
    assert_eq!(
        (usize_arg(&click, "window"), usize_arg(&click, "pane")),
        (Some(1), Some(2))
    );
    assert!(
        sidebar_click_action(strip(23), &t, 4, 9).is_none(),
        "header"
    );
    assert!(
        sidebar_click_action(strip(23), &t, 4, 12).is_none(),
        "blank"
    );
}

/// A queue row commits a LOCAL focus or a CROSS-SESSION re-attach (a
/// graph row by resource identity, never fabricated indices); a roster row
/// switches session (by host when it has one; unreachable is inert); an
/// overflow row opens the fleet dashboard.
#[test]
fn sidebar_queue_and_roster_rows_commit_their_own_actions() {
    let t = targets(2, 2, 0);
    // Row 1 is the filter chips: the `all` chip picks its scope.
    let chip = sidebar_click_action(strip(23), &t, 2, 1).expect("filter chip");
    assert_eq!(chip.action, "agents-filter");
    assert_eq!(str_arg(&chip, "scope").as_deref(), Some("all"));
    let local = sidebar_click_action(strip(23), &t, 4, 2).expect("queue row 0");
    assert_eq!(
        (local.action.as_str(), index_arg(&local)),
        ("select-window", Some(1))
    );
    let peer = sidebar_click_action(strip(23), &t, 4, 3).expect("queue row 1");
    assert_eq!(peer.action, "switch-session");
    assert_eq!(str_arg(&peer, "name").as_deref(), Some("peer-1"));
    assert_eq!(
        (usize_arg(&peer, "window"), usize_arg(&peer, "pane")),
        (Some(2), Some(3))
    );
    assert!(!peer.args.contains_key("resource"));

    let mut graph = t;
    graph.needs_you[1] = SidebarTarget::Session {
        name: "peer".to_owned(),
        id: Some(phux_protocol::ids::SessionId::new(2)),
        window: None,
        pane: None,
        resource: Some(ResourceId::local(10)),
    };
    let click = sidebar_click_action(strip(23), &graph, 4, 3).expect("graph row");
    assert_eq!(str_arg(&click, "resource").as_deref(), Some("@10"));
    assert!(
        !click.args.contains_key("window") && !click.args.contains_key("pane"),
        "{:?}",
        click.args
    );

    let mut t = targets(0, 2, 2);
    let space = sidebar_click_action(strip(23), &t, 4, 13).expect("roster row");
    assert_eq!(space.action, "switch-session");
    assert_eq!(str_arg(&space, "name").as_deref(), Some("space-0"));
    assert!(!space.args.contains_key("pane"));
    t.roster[0].as_mut().expect("row").host = Some("devbox".to_owned());
    let host = sidebar_click_action(strip(23), &t, 4, 13).expect("host row");
    assert_eq!(str_arg(&host, "host").as_deref(), Some("devbox"));
    t.roster[0] = None;
    assert!(
        sidebar_click_action(strip(23), &t, 4, 13).is_none(),
        "unreachable host is inert"
    );

    let t = targets(12, 1, 1);
    assert!(
        (0..23)
            .filter_map(|y| sidebar_click_action(strip(23), &t, 4, y))
            .any(|r| r.action == "agent-fleet")
    );
}

/// Every action a sidebar click commits is a dispatched action name.
#[test]
fn sidebar_click_actions_are_dispatched_names() {
    let t = targets(9, 3, 2);
    let narrow = Rect {
        x: 0,
        y: 0,
        w: 20,
        h: 23,
    };
    for y in 0..23 {
        for x in [2u16, 19] {
            if let Some(resolved) = sidebar_click_action(narrow, &t, x, y) {
                assert!(
                    ACTION_NAMES.contains(&resolved.action.as_str()),
                    "{}",
                    resolved.action
                );
            }
        }
    }
}

/// A left-docked 20-column sidebar over an 80-column viewport and a
/// two-tab workspace ("1", "two"); with the fixed half Agents panel the tab
/// rows sit at 15 and 16 in a 24-row strip.
fn sidebar_env(
    height: u16,
    sidebar_targets: crate::render::chrome::sidebar::SidebarTargets,
) -> Env<'static> {
    let mut fx = CtxFixture::default();
    fx.workspace.add_group("two".to_owned(), tid(2));
    fx.workspace.select(0);
    fx.viewport = (80, height);
    fx.sidebar = Some(SidebarReservation {
        edge: SidebarEdge::Left,
        width: 20,
    });
    fx.sidebar_enabled = true;
    fx.sidebar_targets = Some(sidebar_targets);
    fx.bar = Some(Position::Bottom);
    Env::new(fx)
}

fn window_names(env: &Env<'_>) -> Vec<String> {
    env.fx
        .workspace
        .groups
        .iter()
        .map(|w| w.name.clone())
        .collect()
}

fn drop_at(env: &Env<'_>) -> Option<usize> {
    match &env.fx.drag {
        Some(DragGrab::Window(grab)) => grab.drop_at,
        _ => None,
    }
}

/// End-to-end sidebar clicks: a window block selects it (right press also
/// opens its menu), `+ new` parks a spawn, blank rows are consumed (right
/// press there opens the session menu).
#[tokio::test]
async fn sidebar_clicks_run_their_affordances() {
    for (event, active, overlay, pending) in [
        (left(MouseAction::Press, 3, 16), 1, false, 0),
        (left(MouseAction::Press, 3, 23), 0, false, 1),
        (left(MouseAction::Press, 3, 10), 0, false, 0),
        (right_press(3, 16), 1, true, 0),
        (right_press(3, 10), 0, true, 0),
    ] {
        let mut env = sidebar_env(24, targets(0, 2, 1));
        env.dispatch(vec![event.clone()]).await;
        assert_eq!(env.fx.workspace.active, active, "{event:?}");
        assert_eq!(env.fx.overlays.is_active(), overlay, "{event:?}");
        assert_eq!(env.fx.pending_windows.len(), pending, "{event:?}");
    }
}

#[tokio::test]
async fn sidebar_right_click_manages_the_clicked_session_without_switching() {
    let table = targets(0, 2, 2);
    let mut env = sidebar_env(23, table);
    env.dispatch(vec![right_press(4, 13)]).await;
    assert!(env.fx.switch_request.is_none());
    let InputEvent::Key(enter) = press(PhysicalKey::Enter, None) else {
        unreachable!()
    };
    let crate::render::overlay::OverlayOutcome::RunAction(action) =
        env.fx.overlays.handle_key(&enter)
    else {
        panic!("session actions")
    };
    assert_eq!(action.action, "switch-session");
    assert_eq!(str_arg(&action, "name").as_deref(), Some("space-0"));
}

#[tokio::test]
async fn short_sidebar_overflow_clicks_open_both_navigation_overlays() {
    let table = targets(3, 2, 2);
    for (y, action) in [(2, "agent-fleet"), (3, "session-picker")] {
        let short = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 7,
        };
        assert_eq!(
            sidebar_click_action(short, &table, 4, y)
                .expect(action)
                .action,
            action
        );
        let mut env = sidebar_env(7, table.clone());
        env.dispatch(vec![left(MouseAction::Press, 4, y)]).await;
        assert!(env.fx.overlays.is_active(), "{action} opens an overlay");
        assert!(env.fx.pending_windows.is_empty());
    }
}

/// Dragging the strip's rule (column 19) resizes it to follow the pointer,
/// clamped so the strip keeps a name and the panes keep `min_pane_cols`;
/// release ends the grab, and focus loss abandons it so the next press acts.
#[tokio::test]
async fn dragging_the_sidebar_edge_resizes_the_strip() {
    let grab = left(MouseAction::Press, 19, 5);
    let cases = [
        (
            vec![
                grab.clone(),
                left(MouseAction::Motion, 25, 5),
                left(MouseAction::Motion, 29, 6),
                left(MouseAction::Release, 29, 6),
            ],
            30,
            false,
            0,
        ),
        (
            vec![
                grab.clone(),
                left(MouseAction::Motion, 25, 5),
                InputEvent::Focus(FocusEvent::Lost),
                // The next press acts: it selects (and picks up) its row.
                left(MouseAction::Press, 3, 16),
            ],
            26,
            true,
            1,
        ),
        (
            vec![grab.clone(), left(MouseAction::Motion, 75, 5)],
            40,
            true,
            0,
        ),
        (
            vec![grab, left(MouseAction::Motion, 2, 5)],
            super::chrome_drag::MIN_DRAGGED_SIDEBAR_COLS,
            true,
            0,
        ),
    ];
    for (events, width, live, active) in cases {
        let mut env = sidebar_env(24, targets(0, 2, 1));
        env.dispatch(events).await;
        assert_eq!(env.fx.sidebar_width, width);
        assert_eq!(env.fx.drag.is_some(), live);
        assert_eq!(env.fx.workspace.active, active);
        assert!(env.fx.sidebar_enabled);
    }
    // The rule's bottom corner is the collapse chevron, not a handle.
    let mut env = sidebar_env(24, targets(0, 2, 1));
    env.dispatch(vec![left(MouseAction::Press, 19, 23)]).await;
    assert!(!env.fx.sidebar_enabled && env.fx.drag.is_none());
    assert_eq!(env.fx.sidebar_width, 20);
}

/// Width math for both docks, and a strip floor that yields to a config
/// already narrower than the default floor.
#[test]
fn dragged_sidebar_width_tracks_the_pointer_within_both_floors() {
    use super::chrome_drag::{WidthFloors, dragged_sidebar_width};
    let floors = WidthFloors {
        strip: 16,
        panes: 40,
    };
    for (cols, edge, x, width) in [
        (80, SidebarEdge::Left, 29, Some(30)),
        (80, SidebarEdge::Right, 50, Some(30)),
        (80, SidebarEdge::Left, 79, Some(40)),
        (50, SidebarEdge::Left, 20, None),
    ] {
        assert_eq!(dragged_sidebar_width(cols, edge, x, floors), width);
    }
    let narrow = WidthFloors {
        strip: 12,
        panes: 40,
    };
    assert_eq!(
        dragged_sidebar_width(80, SidebarEdge::Left, 11, narrow),
        Some(12)
    );
}

/// A window row pressed and released over another row moves into that slot,
/// still active; released anywhere else (or clicked) it stays put. Motion
/// over another row marks the drop slot; release or focus loss clears it.
#[tokio::test]
async fn dragging_a_sidebar_window_row_reorders_windows() {
    let grab = left(MouseAction::Press, 3, 15);
    let over = left(MouseAction::Motion, 3, 16);
    let cases = [
        (
            vec![
                grab.clone(),
                over.clone(),
                left(MouseAction::Release, 3, 16),
            ],
            ["two", "1"],
            None,
            false,
        ),
        (
            vec![grab.clone(), left(MouseAction::Release, 3, 15)],
            ["1", "two"],
            None,
            false,
        ),
        (
            vec![grab.clone(), left(MouseAction::Release, 3, 10)],
            ["1", "two"],
            None,
            false,
        ),
        (
            vec![grab.clone(), left(MouseAction::Release, 50, 16)],
            ["1", "two"],
            None,
            false,
        ),
        (
            vec![grab.clone(), over.clone()],
            ["1", "two"],
            Some(1),
            true,
        ),
        (
            vec![grab, over, InputEvent::Focus(FocusEvent::Lost)],
            ["1", "two"],
            None,
            false,
        ),
    ];
    for (events, order, marker, live) in cases {
        let mut env = sidebar_env(24, targets(0, 2, 1));
        env.dispatch(events).await;
        assert_eq!(window_names(&env), order);
        assert_eq!(drop_at(&env), marker);
        assert_eq!(env.fx.drag.is_some(), live);
        if order == ["two", "1"] {
            assert_eq!(
                env.fx.workspace.active, 1,
                "the dragged window stays active"
            );
        }
    }
}
