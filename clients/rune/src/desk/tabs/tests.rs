use super::*;
use phux_client_core::layout::{Group, LayoutState};

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
