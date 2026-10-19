//! User closes kill processes; moves still use the detach-only view path.
use super::*;
use crate::connection::DeterministicDial;
use phux_protocol::wire::frame::{CloseReason, Command, FrameKind};

fn kills(connection: &Connection) -> Vec<ResourceId> {
    connection
        .client
        .take_outbound()
        .iter()
        .filter_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
            FrameKind::Command {
                command: Command::KillResource { terminal_id, .. },
                ..
            } => Some(terminal_id),
            _ => None,
        })
        .collect()
}

#[test]
fn close_pane_queues_a_kill_and_waits_for_confirmed_exit_before_removing_every_mirror() {
    let _dial = DeterministicDial::arm();
    let mut connection = view_tests::attached();
    let mut workspace = view_tests::opened(&mut connection);
    let terminal = workspace.focused().unwrap().terminal_id.clone().unwrap();
    workspace.duplicate_view(&connection);
    let placement = workspace.focused().unwrap().id.clone();
    let before = workspace.tabs.clone();
    let _ = connection.client.take_outbound();

    workspace.terminate_pane(&connection, &placement);

    assert_eq!(kills(&connection), vec![terminal.clone()]);
    assert_eq!(
        workspace.tabs, before,
        "unconfirmed close must not hide a shell"
    );
    connection
        .client
        .feed(FrameKind::ResourceClosed {
            terminal_id: terminal.clone(),
            exit_status: Some(0),
            signal: None,
            reason: CloseReason::Killed,
        })
        .unwrap();
    let events = connection.drain();
    workspace.handle(&events, &connection);
    workspace.settle(&connection);
    assert_eq!(workspace.views_of(&terminal), 0);
    assert!(workspace.tabs.is_empty());
}

#[test]
fn close_tab_kills_a_mirrored_process_only_once() {
    let _dial = DeterministicDial::arm();
    let mut connection = view_tests::attached();
    let mut workspace = view_tests::opened(&mut connection);
    let terminal = workspace.focused().unwrap().terminal_id.clone().unwrap();
    workspace.duplicate_view(&connection);
    let tab = workspace.active_id.clone();
    let _ = connection.client.take_outbound();
    workspace.terminate_tab(&connection, &tab);
    assert_eq!(kills(&connection), vec![terminal]);
    assert_eq!(workspace.tabs.len(), 1, "wait for server confirmation");
}

#[test]
fn close_while_disconnected_keeps_the_pane_and_reports_a_refusal() {
    let _dial = DeterministicDial::arm();
    let mut connection = view_tests::attached();
    let mut workspace = view_tests::opened(&mut connection);
    let placement = workspace.focused().unwrap().id.clone();
    let before = workspace.tabs.clone();
    connection
        .client
        .with_control(phux_client_runtime::control::ControlPlane::connection_opened);
    let _ = connection.client.take_outbound();
    workspace.terminate_pane(&connection, &placement);
    assert!(kills(&connection).is_empty());
    assert_eq!(workspace.tabs, before);
    assert_eq!(workspace.take_notices()[0].title, "Terminate refused");
}

#[test]
fn closing_an_unavailable_saved_tab_removes_the_placeholder_without_claiming_a_kill() {
    let _dial = DeterministicDial::arm();
    let mut connection = view_tests::attached();
    let mut workspace = view_tests::opened(&mut connection);
    let placement = Placement {
        id: "stale".into(),
        terminal_id: Some(ResourceId::local(99)),
        terminal_label: "local:99".into(),
        view_id: None,
    };
    workspace.tabs.push(DeskTab {
        id: "saved".into(),
        title: None,
        root: LayoutNode::Leaf { placement },
        focused_id: "stale".into(),
        zoomed_id: None,
    });
    workspace.unresolved.insert("stale".into());
    let _ = connection.client.take_outbound();
    workspace.terminate_tab(&connection, "saved");
    assert_eq!(workspace.tabs.len(), 1);
    assert!(!workspace.unresolved.contains("stale"));
    assert!(kills(&connection).is_empty());
    assert_eq!(
        workspace.take_notices()[0].title,
        "Unavailable view removed"
    );
}

#[test]
fn shell_exit_removes_the_pane_without_sending_a_kill() {
    let _dial = DeterministicDial::arm();
    let mut connection = view_tests::attached();
    let mut workspace = view_tests::opened(&mut connection);
    let terminal = workspace.focused().unwrap().terminal_id.clone().unwrap();
    connection
        .client
        .feed(FrameKind::ResourceClosed {
            terminal_id: terminal,
            exit_status: Some(0),
            signal: None,
            reason: CloseReason::Unknown,
        })
        .unwrap();
    let events = connection.drain();
    workspace.handle(&events, &connection);
    workspace.settle(&connection);
    assert!(workspace.tabs.is_empty());
    assert!(kills(&connection).is_empty());
}
