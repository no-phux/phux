use super::*;
use phux_client_core::layout::{Group, LayoutNode, LayoutState};
use phux_protocol::wire::frame::FrameKind;

fn id(n: u32) -> ResourceId {
    ResourceId::local(n)
}

fn doc() -> ArrangementDoc {
    ArrangementDoc {
        groups: vec![
            Group::new("1".to_owned(), LayoutState::single(id(1))),
            Group::new("2".to_owned(), LayoutState::single(id(2))),
            Group::new("gone".to_owned(), LayoutState::single(id(9))),
        ],
        active: 0,
        slots: Vec::new(),
    }
}

#[test]
fn the_active_tab_is_the_one_holding_the_focus() {
    let mut d = doc();
    focus_group(&mut d, Some(&id(2)));
    assert_eq!(d.active, 1);
    focus_group(&mut d, Some(&id(7)));
    assert_eq!(d.active, 1);
}

#[test]
fn a_group_with_no_live_terminal_is_not_a_tab() {
    let tabs = list(&doc(), &[id(1), id(2)]);
    assert_eq!(tabs.len(), 2);
    assert!(tabs[0].active && !tabs[1].active);
    assert_eq!(tabs[1].resources, vec![id(2)]);
}

#[test]
fn sidebar_main_press_keeps_stable_identity_but_returns_to_the_remembered_member() {
    let replica = crate::test_support::terminal::Replica::new(120, 40, 3);
    let mut desk = crate::test_support::window();
    let session = crate::test_support::server::session(1, "work", 1);
    let panes = (1..=3)
        .map(|n| crate::test_support::server::pane(n, &session, format!("Work {n}"), None))
        .collect();
    let mut topology = crate::desk::topology(vec![session], panes);
    crate::test_support::server::terminal_resources(&mut topology);
    desk.connection.topology = Some(topology);
    desk.connection.client = Some(replica.client.clone());
    desk.connection.shown = Some(super::super::Shown {
        resource: id(1),
        view: Some(replica.views[0]),
    });
    desk.connection.layout = Some(LayoutNode::Leaf(id(1)));
    let tab = Tab {
        id: [1; 16],
        name: "Navigation".to_owned(),
        resources: [1, 2, 3].map(id).to_vec(),
        active: true,
        locked: false,
    };
    desk.connection.tabs.focus.insert(tab.id, id(3));
    desk.connection.tabs.by_session.insert(1, vec![tab]);
    desk.sidebar_open = true;
    let mut rt = crate::test_support::runtime(
        desk,
        crate::ui::view,
        (1100.0, 720.0),
        rune::Env::new(true, 1.0),
    );
    crate::test_support::server::settle(&mut rt);
    let row = rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some("pane-@1") && e.focusable)
        .expect("stable lead");
    let (x, y) = (row.rect[0] + 8.0, row.rect[1] + 8.0);
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(3.0);
    assert!(
        rt.state
            .connection
            .wanted
            .as_ref()
            .is_some_and(|(wanted, _)| *wanted == id(3))
            || rt
                .state
                .connection
                .shown
                .as_ref()
                .is_some_and(|s| s.resource == id(3))
    );
    assert!(
        rt.elements()
            .iter()
            .any(|e| e.key.as_deref() == Some("pane-@1") && e.focusable)
    );
    assert!(rt.state.connection.reorder.held.is_none());
    for bytes in replica.client.take_outbound() {
        assert!(!matches!(
            phux_wire_fixtures::decode_one(&bytes),
            FrameKind::InputKey { .. } | FrameKind::InputPaste { .. }
        ));
    }
}
