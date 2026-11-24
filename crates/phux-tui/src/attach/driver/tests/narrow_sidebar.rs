//! The sidebar on a terminal too narrow for it.

use super::*;

/// On a narrow terminal with `rail = false` the sidebar yields: panes own
/// the whole width, no strip paints, and reflow, mouse routing, and the
/// divider all use the same yielded reservation (a stale strip would put the
/// divider near col 35).
#[test]
fn shipped_frame_yields_the_sidebar_on_a_narrow_terminal() {
    use crate::layout::{LayoutNode, LayoutState, SplitDir, split_at};
    use crate::multi_pane::{RouteDecision, route_mouse_event};
    use crate::render::ChromeBreakpoints;
    use crate::render::chrome::status_bar::Position;
    use phux_protocol::input::key::ModSet;
    use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};

    let view = (50u16, 12u16);
    let min_pane = ChromeBreakpoints::DEFAULT.min_pane_cols;
    let sidebar = sidebar_reservation(view.0, true, false, 20, SidebarEdge::Left, min_pane);
    assert!(sidebar.is_none());
    assert!(sidebar_reservation(view.0, true, false, 0, SidebarEdge::Left, min_pane).is_none());

    let rows = shipped_frame_rows(view, &[probe_window("nvim", true)], sidebar);
    let frame = rows.join("\n");
    assert!(
        rows.iter().any(|r| r.starts_with(PROBE_PANE_TEXT)),
        "{frame}"
    );
    assert!(!frame.contains(PROBE_SESSION), "{frame}");
    let rail = rows.iter().find(|r| r.contains('─')).expect("pane rail");
    assert!(
        rail.starts_with('─') && rail.chars().count() == usize::from(view.0),
        "{rail:?}"
    );

    let content = content_rect(view, Some(Position::Top), sidebar);
    assert_eq!((content.x, content.w), (0, view.0));
    let id = ResourceId::local(1);
    let workspace = ArrangementDoc::single(id.clone());
    let pane_rect = view_rects(&workspace, None, content, view)[&id];
    assert_eq!((pane_rect.x, pane_rect.w), (0, view.0));
    let press_at = |x: u16| MouseEvent {
        action: MouseAction::Press,
        button: MouseButton::Left,
        mods: ModSet::empty(),
        x: f64::from(x),
        y: f64::from(content.y),
    };
    let ls = workspace.render_group(None).expect("layout");
    assert!(matches!(
        route_mouse_event(ls.as_ref(), content, view, &press_at(0)),
        RouteDecision::Pane { target, .. } if target == id
    ));
    let right = ResourceId::local(2);
    let split = LayoutState {
        tree: Some(
            split_at(
                &LayoutNode::Leaf(id.clone()),
                &id,
                &right,
                SplitDir::Horizontal,
                0.5,
            )
            .expect("split"),
        ),
        focus: Some(id),
    };
    let divider_x = (0..view.0)
        .find(|&x| {
            matches!(
                route_mouse_event(&split, content, view, &press_at(x)),
                RouteDecision::Divider { .. }
            )
        })
        .expect("a divider column");
    assert!((20..30).contains(&divider_x), "{divider_x}");
}
