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

#[test]
fn close_acknowledged_spawn_before_bootstrap_kills_instead_of_dismissing() {
    for whole_tab in [false, true] {
        let _dial = DeterministicDial::arm();
        let mut connection = view_tests::attached();
        let mut workspace = view_tests::opened(&mut connection);
        let terminal = ResourceId::local(9);
        let request_id = workspace
            .launch_capture(&connection, SpawnRequest::default(), true)
            .unwrap();
        connection
            .client
            .feed(FrameKind::ResourceSpawned {
                request_id,
                result: phux_protocol::wire::frame::SpawnResult::Ok(terminal.clone()),
            })
            .unwrap();
        let events = connection.drain();
        workspace.handle(&events, &connection);
        workspace.settle(&connection);
        assert!(connection.pane(&terminal).is_none());
        assert!(!connection.ready(&terminal));
        if whole_tab {
            workspace.duplicate_view(&connection);
        }
        let placement = workspace.focused().unwrap().id.clone();
        let before = workspace.tabs.clone();
        let tab = workspace.active_id.clone();
        let _ = connection.client.take_outbound();
        if whole_tab {
            workspace.terminate_tab(&connection, &tab);
        } else {
            workspace.terminate_pane(&connection, &placement);
        }
        assert_eq!(kills(&connection), vec![terminal]);
        assert_eq!(
            workspace.tabs, before,
            "keep the process visible until confirmed exit"
        );
        assert!(workspace.take_notices().is_empty());
    }
}

#[test]
fn closing_a_fresh_split_kills_it_before_a_topology_read_lists_it() {
    let _dial = DeterministicDial::arm();
    let mut connection = view_tests::attached();
    let mut workspace = view_tests::opened(&mut connection);
    workspace.split(&connection, Axis::Row, false);
    let request_id = connection
        .client
        .take_outbound()
        .iter()
        .find_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
            FrameKind::SpawnResource { request_id, .. } => Some(request_id),
            _ => None,
        })
        .expect("split spawns");
    let split = ResourceId::local(8);
    connection
        .client
        .feed(FrameKind::ResourceSpawned {
            request_id,
            result: phux_protocol::wire::frame::SpawnResult::Ok(split.clone()),
        })
        .unwrap();
    let stream = phux_protocol::ids::StreamId::new(2).unwrap();
    let bootstrap = phux_protocol::ids::BootstrapId::new(2).unwrap();
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: split.clone(),
            stream_id: stream,
            bootstrap_id: bootstrap,
            profile: phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 20,
            rows: 4,
            base_seq: 0,
        },
        FrameKind::BootstrapReady {
            terminal_id: split.clone(),
            stream_id: stream,
            bootstrap_id: bootstrap,
            history_cursor: None,
        },
    ] {
        connection.client.feed(frame).unwrap();
    }
    let events = connection.drain();
    workspace.handle(&events, &connection);
    workspace.settle(&connection);
    let placement = workspace.focused().unwrap().clone();
    assert_eq!(placement.terminal_id.as_ref(), Some(&split));
    let _ = connection.client.take_outbound();

    workspace.terminate_pane(&connection, &placement.id);

    assert!(workspace.take_notices().is_empty());
    assert_eq!(kills(&connection), vec![split]);
}
