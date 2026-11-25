use phux_client_runtime::control::{Event, PaneDescriptor};
use phux_protocol::ids::ResourceId;

use super::*;
use crate::mobile::Platform;

fn pane(n: u32) -> PaneDescriptor {
    PaneDescriptor {
        terminal_id: ResourceId::local(n),
        session_id: 1,
        session_name: "work".to_owned(),
        cols: 80,
        rows: 24,
        title: None,
        cwd: None,
        is_focused: false,
        key: None,
        revision: None,
        name: Some(format!("pane {n}")),
        activation: None,
        exec: None,
        foreground: None,
        icon: None,
        parent: None,
        unseen: None,
    }
}

fn phone() -> Desk {
    let mut desk = crate::test_support::window().with_phone(true);
    desk.native = Some(Platform::test());
    desk.machines.current = Some("mini".to_owned());
    let panes = (1..=5).map(pane).collect();
    desk.connection.topology = Some(crate::desk::topology(Vec::new(), panes));
    desk.connection.status = Status::Attached;
    desk
}

fn agent(desk: &mut Desk, n: u32, state: &str) {
    let record = format!(r#"{{"name":"claude","state":"{state}"}}"#);
    let event = Event::AgentMetadata {
        terminal_id: ResourceId::local(n),
        value: Some(record.into_bytes()),
    };
    desk.activities.apply(&event, &[]);
}

#[test]
fn waiting_leads_working_follows_and_rows_open_their_pane() {
    let mut desk = phone();
    for n in [1, 3, 4] {
        agent(&mut desk, n, "working");
    }
    agent(&mut desk, 2, "blocked");
    let shown = desk.take_presence().expect("agents moved");
    assert_eq!((shown.waiting, shown.working), (1, 3));
    assert_eq!(shown.headline, "1 needs you");
    assert_eq!(shown.host, "mini");
    assert_eq!(shown.rows.len(), ROWS);
    assert_eq!(
        shown.rows[0],
        Row {
            title: "pane 2".to_owned(),
            detail: "claude · work".to_owned(),
            state: "needs_you",
            link: "phux://pane/@2".to_owned(),
        }
    );
    assert_eq!(shown.rows[1].state, "working");
}

#[test]
fn it_is_taken_once_per_change_and_only_while_attached() {
    let mut desk = phone();
    agent(&mut desk, 1, "working");
    assert_eq!(desk.take_presence().unwrap().headline, "1 working");
    assert_eq!(desk.take_presence(), None, "nothing moved");
    desk.connection.status = Status::Idle;
    agent(&mut desk, 1, "idle");
    assert_eq!(desk.take_presence(), None, "a dropped link says nothing");
    desk.connection.status = Status::Attached;
    let quiet = desk.take_presence().unwrap();
    assert_eq!((quiet.waiting, quiet.working), (0, 0));
    assert!(quiet.headline.is_empty() && quiet.rows.is_empty());
}

#[test]
fn a_desktop_window_tells_no_phone() {
    let mut desk = phone();
    desk.native = None;
    agent(&mut desk, 1, "working");
    assert_eq!(desk.take_presence(), None);
}
