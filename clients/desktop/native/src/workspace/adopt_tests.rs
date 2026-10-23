//! A session's terminals become tabs without a click, and only once.
use super::view_tests::attached_with;
use super::*;
use crate::connection::DeterministicDial;

const SESSION: &str = "views";

fn home() -> ResourceId {
    ResourceId::local(7)
}

fn agent() -> ResourceId {
    ResourceId::local(8)
}

fn settled(options: WorkspaceOptions, connection: &mut Connection) -> Workspace {
    let mut workspace = Workspace::new(SESSION.into(), options);
    workspace.restore(None);
    let events = connection.drain();
    workspace.handle(&events, connection);
    workspace.settle(connection);
    workspace
}

fn shown(workspace: &Workspace, terminal: &ResourceId) -> bool {
    workspace.views_of(terminal) > 0
}

#[test]
fn a_sessions_other_terminals_open_as_tabs_behind_the_home_tab() {
    let _dial = DeterministicDial::arm();
    let mut connection = attached_with(&[agent()]);
    assert!(connection.ready(&agent()), "fixture streams the agent");
    let workspace = settled(WorkspaceOptions::default(), &mut connection);
    assert_eq!(
        workspace.tabs.len(),
        2,
        "the agent has a tab without a click"
    );
    assert!(shown(&workspace, &home()));
    assert!(shown(&workspace, &agent()));
    let active = workspace
        .focused()
        .and_then(|placement| placement.terminal_id.clone());
    assert_eq!(active, Some(home()), "adoption never takes focus");
}

#[test]
fn a_terminal_moved_out_of_the_window_is_not_pulled_back() {
    let _dial = DeterministicDial::arm();
    let mut connection = attached_with(&[agent()]);
    let mut workspace = settled(WorkspaceOptions::default(), &mut connection);
    let placement = workspace
        .all_placements()
        .find(|placement| placement.terminal_id == Some(agent()))
        .map(|placement| placement.id.clone());
    workspace.close_pane(&connection, placement.as_deref());
    assert!(!shown(&workspace, &agent()));
    workspace.settle(&connection);
    assert!(!shown(&workspace, &agent()));
}

#[test]
fn a_fresh_window_shows_only_its_own_terminal() {
    let _dial = DeterministicDial::arm();
    let mut connection = attached_with(&[agent()]);
    let options = WorkspaceOptions {
        fresh: true,
        initial_terminal: None,
    };
    let workspace = settled(options, &mut connection);
    assert!(!shown(&workspace, &agent()));
}

#[test]
fn adoption_waits_for_pending_spawns_and_skips_background_ones() {
    let _dial = DeterministicDial::arm();
    let mut connection = attached_with(&[agent()]);
    let mut workspace = Workspace::new(SESSION.into(), WorkspaceOptions::default());
    workspace.restore(None);
    workspace.spawns.insert(41, Destination::Background);
    let events = connection.drain();
    workspace.handle(&events, &connection);
    workspace.settle(&connection);
    assert!(
        !shown(&workspace, &agent()),
        "an unanswered spawn could be this terminal"
    );
    workspace.answered(41, Some(&agent()), None);
    workspace.settle(&connection);
    assert!(
        !shown(&workspace, &agent()),
        "a background capture stays in the background"
    );
    assert!(shown(&workspace, &home()));
}
