//! Home-session, spawn-destination and pending-view regressions.
use super::*;

#[test]
fn unrelated_output_does_not_promote_pending_spawns_or_restored_views() {
    let mut workspace = Workspace::new("Intake".to_owned(), WorkspaceOptions::default());
    let event = |id| Event::TerminalChanged {
        terminal_id: ResourceId::local(id),
    };
    workspace.spawns.insert(1, Destination::Tab);
    assert!(!workspace.awaiting_output(&[event(1)]));
    workspace
        .awaiting
        .insert(terminal_label(&ResourceId::local(2)), Destination::Tab);
    assert!(!workspace.awaiting_output(&[event(1)]));
    assert!(workspace.awaiting_output(&[event(2)]));
    let placement = Placement {
        id: "restored-placement".into(),
        terminal_id: Some(ResourceId::local(3)),
        terminal_label: "@3".into(),
        view_id: None,
    };
    workspace.unresolved.insert(placement.id.clone());
    workspace.tabs.push(DeskTab {
        id: "tab".into(),
        title: None,
        focused_id: placement.id.clone(),
        zoomed_id: None,
        root: leaf(placement),
    });
    assert!(workspace.awaiting_output(&[event(3)]));
    assert!(!workspace.awaiting_output(&[event(4)]));
    workspace.unresolved.clear();
    assert!(!workspace.awaiting_output(&[event(3)]));
}

fn offline() -> (crate::connection::DeterministicDial, Connection) {
    let dial = crate::connection::DeterministicDial::arm();
    let connection =
        Connection::connect_with_key("/isolated-workspace.sock".into(), "main".into(), None)
            .unwrap();
    (dial, connection)
}

fn one_pane_tab(tab: &str, terminal: u32) -> DeskTab {
    let placement = Placement {
        id: format!("{tab}-place"),
        terminal_id: Some(ResourceId::local(terminal)),
        terminal_label: terminal_label(&ResourceId::local(terminal)),
        view_id: None,
    };
    DeskTab {
        id: tab.into(),
        title: None,
        focused_id: placement.id.clone(),
        zoomed_id: None,
        root: leaf(placement),
    }
}

fn split_of(tab: &str) -> Destination {
    Destination::Split {
        tab_id: tab.into(),
        target_id: format!("{tab}-place"),
        axis: Axis::Row,
        before: false,
    }
}

/// One spawn reply followed by the per-terminal step of
/// [`Workspace::settle`]. `settle` itself returns early on a connection
/// that is not attached and validated, and no deterministic attached
/// connection exists for unit tests, so these drive the same
/// `answered` -> `land_answered` path.
fn reply(workspace: &mut Workspace, connection: &Connection, request: u32, terminal: u32) {
    let terminal = ResourceId::local(terminal);
    workspace.answered(request, Some(&terminal), None);
    workspace.land_answered(connection);
}

#[test]
fn an_answered_spawn_lands_before_the_terminal_accepts_input() {
    let (_dial, connection) = offline();
    let terminal = ResourceId::local(9);
    assert!(!connection.ready(&terminal));
    let mut workspace = Workspace::new("main".into(), WorkspaceOptions::default());
    workspace.tabs = vec![one_pane_tab("a", 1)];
    workspace.active_id = "a".into();
    workspace.spawns.insert(7, split_of("a"));
    workspace.answered(7, Some(&terminal), None);
    workspace.land_answered(&connection);
    assert_eq!(leaves(&workspace.tabs[0]), ["local:1", "local:9"]);
    assert_eq!(workspace.pending(), 0);
    let placed = placements(&workspace.tabs[0].root)
        .into_iter()
        .find(|placement| placement.terminal_label == "local:9")
        .unwrap();
    assert!(placed.view_id.is_none());
    assert!(workspace.unresolved.contains(&placed.id));
}

fn leaves(tab: &DeskTab) -> Vec<String> {
    placements(&tab.root)
        .into_iter()
        .map(|placement| placement.terminal_label.clone())
        .collect()
}

#[test]
fn pending_split_lands_in_its_original_tab_not_the_later_focus() {
    let (_dial, connection) = offline();
    let mut workspace = Workspace::new("main".into(), WorkspaceOptions::default());
    workspace.tabs = vec![one_pane_tab("a", 1), one_pane_tab("b", 2)];
    workspace.active_id = "a".into();
    workspace.spawns.insert(7, split_of("a"));
    workspace.select_tab("b");
    reply(&mut workspace, &connection, 7, 9);
    assert_eq!(leaves(&workspace.tabs[0]), ["local:1", "local:9"]);
    assert_eq!(leaves(&workspace.tabs[1]), ["local:2"]);
    assert_eq!(workspace.active_id, "a");
}

#[test]
fn closing_a_pending_split_destination_never_splits_the_focused_pane() {
    let (_dial, connection) = offline();
    let mut workspace = Workspace::new("main".into(), WorkspaceOptions::default());
    workspace.tabs = vec![one_pane_tab("a", 1), one_pane_tab("b", 2)];
    workspace.active_id = "a".into();
    workspace.spawns.insert(7, split_of("a"));
    workspace.close_tab(&connection, "a");
    assert_eq!(workspace.active_id, "b");
    reply(&mut workspace, &connection, 7, 9);
    // The surviving focused tab is untouched; the orphan gets its own tab
    // instead of being lost (its process is already running).
    assert_eq!(workspace.tabs.len(), 2);
    assert_eq!(leaves(&workspace.tabs[0]), ["local:2"]);
    assert_eq!(leaves(&workspace.tabs[1]), ["local:9"]);
    assert_eq!(workspace.pending(), 0);
}

/// The layout is never changed optimistically when a split is requested,
/// so "restore the previous layout" holds by construction; what a refusal
/// must get right is dropping the destination so nothing lands later.
#[test]
fn refused_split_drops_its_destination_and_keeps_layout_and_focus() {
    let mut workspace = Workspace::new("main".into(), WorkspaceOptions::default());
    workspace.tabs = vec![one_pane_tab("a", 1), one_pane_tab("b", 2)];
    workspace.active_id = "b".into();
    let before = (workspace.tabs.clone(), workspace.active_id.clone());
    workspace.spawns.insert(7, split_of("b"));
    workspace.answered(7, None, Some("refused"));
    assert_eq!(workspace.pending(), 0);
    let notices = workspace.take_notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].title, "Could not start a terminal");
    // A late or duplicated reply for the refused request has no
    // destination left, so it cannot split the focused pane.
    workspace.answered(7, Some(&ResourceId::local(9)), None);
    assert!(workspace.awaiting.is_empty());
    assert_eq!(
        (workspace.tabs.clone(), workspace.active_id.clone()),
        before
    );
}

#[test]
fn ready_tab_spawn_for_a_shown_terminal_reveals_instead_of_landing_twice() {
    let (_dial, connection) = offline();
    let mut workspace = Workspace::new("main".into(), WorkspaceOptions::default());
    workspace.tabs = vec![one_pane_tab("a", 1), one_pane_tab("b", 2)];
    workspace.active_id = "b".into();
    workspace.spawns.insert(7, Destination::Tab);
    reply(&mut workspace, &connection, 7, 1);
    assert_eq!(workspace.tabs.len(), 2);
    assert_eq!(workspace.active_id, "a");
    assert_eq!(workspace.pending(), 0);
}

#[test]
fn home_session_name_tracks_the_same_renamed_session() {
    let mut workspace = Workspace::new("Intake".to_owned(), WorkspaceOptions::default());
    workspace.set_home_session("Unorganized".to_owned());
    assert_eq!(workspace.session, "Unorganized");
    assert!(workspace.tabs.is_empty());
}
