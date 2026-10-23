//! Fleet selection must outlive the cold workspace's default placement.

use super::*;
use phux_client_runtime::control::{ControlPlane, Event};
use phux_protocol::{
    PROTOCOL_VERSION,
    caps::{
        BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
        ServerFeature, ServerFeatureSet,
    },
    ids::{BootstrapId, ClientId, SessionId, StreamId, WindowId},
    wire::{
        frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind},
        info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo},
    },
};

const SESSION: &str = "cold-fleet";
const DEFAULT: u32 = 7;
const SELECTED: u32 = 8;

fn snapshot(terminals: &[u32]) -> SessionSnapshot {
    SessionSnapshot::new(
        SessionId::new(1),
        WindowId::new(1),
        ResourceId::local(DEFAULT),
    )
    .with_sessions(vec![SessionInfo::new(SessionId::new(1), SESSION)])
    .with_windows(vec![WindowInfo::new(
        WindowId::new(1),
        SessionId::new(1),
        "agents",
    )])
    // Inventory order stays fixed even when bootstrap order is reversed.
    .with_resources(
        [DEFAULT, SELECTED]
            .into_iter()
            .filter(|id| terminals.contains(id))
            .map(|id| ResourceInfo::new(ResourceId::local(id), WindowId::new(1), 20, 4))
            .collect(),
    )
}

fn feed_bootstrap(client: &phux_client_runtime::Client, id: u32) {
    let terminal_id = ResourceId::local(id);
    let stream_id = StreamId::new(u64::from(id)).unwrap();
    let bootstrap_id = BootstrapId::new(u64::from(id)).unwrap();
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 20,
            rows: 4,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: format!("agent {id}").into_bytes().into(),
        },
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ] {
        client.feed(frame).unwrap();
    }
}

/// Use the real embedded runtime, not a synthetic Attached status: placements
/// must acquire actual views, and readiness must come from bootstrap frames.
fn attached(order: &[u32]) -> Connection {
    let mut connection =
        Connection::connect("/isolated-fleet-bootstrap.sock".into(), SESSION.into()).unwrap();
    let client = std::sync::Arc::clone(&connection.client);
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_features(ServerFeatureSet::with(&[ServerFeature::AcknowledgedInput])),
            server_id: vec![1; 16],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        })
        .unwrap();
    let attach_id = client
        .take_outbound()
        .iter()
        .find_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .expect("session attach request");
    client
        .feed(FrameKind::Attached {
            attach_id,
            snapshot: snapshot(order),
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
    for &id in order {
        feed_bootstrap(&client, id);
    }
    // The initial attach barrier requires every listed terminal's bootstrap.
    // The workspace is deliberately still cold when the click is processed.
    client.feed(FrameKind::AttachReady { attach_id }).unwrap();
    if !order.contains(&SELECTED) {
        // A terminal appearing after the initial session attach is listed but
        // not streamed yet. Unlike omitting an initial participant's bootstrap,
        // this permits a real Attached connection with a not-ready target.
        let request_id = client.refresh_topology().expect("topology request");
        client
            .feed(FrameKind::CommandResult {
                request_id,
                result: CommandResult::OkWith(CommandValue::State(snapshot(&[DEFAULT, SELECTED]))),
            })
            .unwrap();
    }
    connection.drain();
    let _ = client.take_outbound();
    assert_eq!(connection.status, Status::Attached);
    assert!(connection.ready(&ResourceId::local(DEFAULT)));
    assert_eq!(
        connection.ready(&ResourceId::local(SELECTED)),
        order.contains(&SELECTED)
    );
    connection.agents.insert(
        ResourceId::local(SELECTED),
        crate::connection::AgentInfo {
            name: "Selected agent".into(),
            kind: Some("opencode".into()),
            state: AgentState::Working,
            attention: crate::connection::AgentAttention::None,
            changed_at_ms: 1,
        },
    );
    connection
}

fn settle(desk: &mut Desk) {
    let events = desk.link.as_mut().unwrap().connection.drain();
    for event in &events {
        desk.note_fleet_event(event);
    }
    let link = desk.link.as_mut().unwrap();
    link.workspace.handle(&events, &link.connection);
    link.workspace.settle(&link.connection);
}

fn focused(desk: &Desk) -> Option<ResourceId> {
    desk.link
        .as_ref()
        .unwrap()
        .workspace
        .focused()
        .and_then(|placement| placement.terminal_id.clone())
}

fn selection_survives_restore(cx: &mut TestAppContext, order: [u32; 2]) {
    let _dial = DeterministicDial::arm();
    let (desk, _) = fixture(cx);
    desk.update(cx, |desk, _| {
        install(desk, attached(&order));
        let workspace = &mut desk.link.as_mut().unwrap().workspace;
        workspace.restore(None);
        assert!(!workspace.restored());
        assert!(workspace.tabs.is_empty());

        let target = desk
            .agent_catalog()
            .into_iter()
            .find(|agent| agent.terminal == ResourceId::local(SELECTED))
            .expect("selected terminal comes from live catalog");
        assert!(desk.work_is_active(&target.destination));
        desk.fleet_view.pending = Some(target);
        // A fleet click can reveal the terminal before the first workspace
        // settle. It must not consume the intent just because the view exists.
        desk.settle_fleet_terminal();
        assert_eq!(focused(desk), Some(ResourceId::local(SELECTED)));

        settle(desk);
        assert_eq!(
            focused(desk),
            Some(ResourceId::local(DEFAULT)),
            "the cold restore really does place its first/default pane last"
        );
        desk.settle_fleet_terminal();
        assert_eq!(
            focused(desk),
            Some(ResourceId::local(SELECTED)),
            "fleet selection must win over initial default placement: {order:?}"
        );
        assert!(desk.fleet_view.pending.is_none());
        let workspace = &desk.link.as_ref().unwrap().workspace;
        assert!(workspace.restored());
        assert_eq!(workspace.pending(), 0);
        assert_eq!(workspace.views_of(&ResourceId::local(DEFAULT)), 1);
        assert_eq!(workspace.views_of(&ResourceId::local(SELECTED)), 1);
        assert!(workspace.focused().unwrap().view_id.is_some());

        settle(desk);
        desk.settle_fleet_terminal();
        assert_eq!(focused(desk), Some(ResourceId::local(SELECTED)));
        assert_eq!(desk.link.as_ref().unwrap().workspace.tabs.len(), 2);
    });
}

#[gpui::test]
fn nonfirst_fleet_selection_survives_default_bootstrapping_first(cx: &mut TestAppContext) {
    selection_survives_restore(cx, [DEFAULT, SELECTED]);
}

#[gpui::test]
fn nonfirst_fleet_selection_survives_selected_bootstrapping_first(cx: &mut TestAppContext) {
    selection_survives_restore(cx, [SELECTED, DEFAULT]);
}

#[gpui::test]
fn closing_selected_tab_during_cold_restore_does_not_reopen_it(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (desk, _) = fixture(cx);
    desk.update(cx, |desk, _| {
        install(desk, attached(&[DEFAULT, SELECTED]));
        let target = desk
            .agent_catalog()
            .into_iter()
            .find(|agent| agent.terminal == ResourceId::local(SELECTED))
            .unwrap();
        desk.fleet_view.pending = Some(target);
        desk.settle_fleet_terminal();
        assert_eq!(focused(desk), Some(ResourceId::local(SELECTED)));
        assert!(desk.fleet_view.pending.is_some());
        let workspace = &desk.link.as_ref().unwrap().workspace;
        assert!(
            !workspace.restored(),
            "selection still competes with restore"
        );
        let selected_tab = workspace.active_id.clone();

        // Exercise the pointer-facing Desk method, not Workspace::close_tab.
        desk.close_tab(&selected_tab);
        // Closing now waits for the server to confirm process termination.
        assert!(desk.fleet_view.pending.is_none());
        desk.link
            .as_ref()
            .unwrap()
            .connection
            .client
            .feed(FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(SELECTED),
                exit_status: Some(0),
                signal: None,
                reason: phux_protocol::wire::frame::CloseReason::Killed,
            })
            .unwrap();
        for _ in 0..2 {
            settle(desk);
            desk.settle_fleet_terminal();
            assert_eq!(focused(desk), Some(ResourceId::local(DEFAULT)));
            let workspace = &desk.link.as_ref().unwrap().workspace;
            assert_eq!(workspace.views_of(&ResourceId::local(SELECTED)), 0);
            assert_eq!(
                workspace.tabs.len(),
                1,
                "closed target must not be reopened"
            );
        }
        assert!(desk.fleet_view.pending.is_none());
    });
}

fn late_bootstrap_after_navigation(cx: &mut TestAppContext, preexisting: bool, later_open: bool) {
    let _dial = DeterministicDial::arm();
    let (desk, _) = fixture(cx);
    desk.update(cx, |desk, _| {
        install(desk, attached(&[DEFAULT]));
        desk.link.as_mut().unwrap().workspace.restore(None);
        settle(desk);
        let default_tab = desk.link.as_ref().unwrap().workspace.active_id.clone();
        let selected = ResourceId::local(SELECTED);
        if preexisting {
            desk.reveal_terminal(&selected);
        }
        desk.fleet_view.pending = Some(desk.agent_catalog()[0].clone());
        desk.settle_fleet_terminal();
        assert!(
            desk.link
                .as_ref()
                .unwrap()
                .workspace
                .pending_open(&selected)
        );
        if later_open {
            desk.reveal_terminal(&selected);
        }
        let preserve_open = preexisting || later_open;
        desk.select_tab(&default_tab);
        assert!(desk.fleet_view.pending.is_none());
        assert_eq!(
            desk.link
                .as_ref()
                .unwrap()
                .workspace
                .pending_open(&selected),
            preserve_open,
            "only the fleet-owned open is withdrawn"
        );
        let connection = &desk.link.as_ref().unwrap().connection;
        for request_id in take_selected_attaches(connection) {
            connection
                .client
                .feed(FrameKind::CommandResult {
                    request_id,
                    result: CommandResult::Ok,
                })
                .unwrap();
        }
        feed_bootstrap(&connection.client, SELECTED);
        settle(desk);
        desk.settle_fleet_terminal();
        if !preserve_open {
            assert_eq!(
                focused(desk),
                Some(ResourceId::local(DEFAULT)),
                "late bootstrap cannot steal focus"
            );
            assert_eq!(
                desk.link.as_ref().unwrap().workspace.views_of(&selected),
                1,
                "automatic home-session adoption still lands a quiet tab"
            );
        } else {
            assert_eq!(
                desk.link.as_ref().unwrap().workspace.views_of(&selected),
                1,
                "a preexisting non-fleet open is not silently discarded"
            );
        }
    });
}

#[gpui::test]
fn later_tab_choice_withdraws_fleet_open_before_delayed_bootstrap(cx: &mut TestAppContext) {
    late_bootstrap_after_navigation(cx, false, false);
}

#[gpui::test]
fn cancelling_a_fleet_choice_preserves_a_preexisting_open(cx: &mut TestAppContext) {
    late_bootstrap_after_navigation(cx, true, false);
}

#[gpui::test]
fn a_later_explicit_open_takes_ownership_from_the_fleet(cx: &mut TestAppContext) {
    late_bootstrap_after_navigation(cx, false, true);
}

fn take_selected_attaches(connection: &Connection) -> Vec<u32> {
    connection
        .client
        .take_outbound()
        .iter()
        .filter_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
            FrameKind::Command {
                request_id,
                command: Command::AttachResource { terminal_id, .. },
            } if terminal_id == ResourceId::local(SELECTED) => Some(request_id),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn refused_fleet_attach_is_not_retried_and_keeps_workspace_notice(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (desk, _) = fixture(cx);
    desk.update(cx, |desk, _| {
        install(desk, attached(&[DEFAULT]));
        let target = desk
            .agent_catalog()
            .into_iter()
            .find(|agent| agent.terminal == ResourceId::local(SELECTED))
            .expect("new same-session terminal is inventoried before bootstrap");
        desk.fleet_view.pending = Some(target);
        desk.settle_fleet_terminal();
        let link = desk.link.as_ref().unwrap();
        assert_eq!(link.workspace.pending(), 1);
        assert!(!link.connection.ready(&ResourceId::local(SELECTED)));
        let requests = take_selected_attaches(&link.connection);
        assert_eq!(requests.len(), 1, "one real explicit attach was queued");
        link.connection
            .client
            .feed(FrameKind::CommandResult {
                request_id: requests[0],
                result: CommandResult::Error {
                    code: ErrorCode::InvalidCommand,
                    message: "fixture denies attach".into(),
                },
            })
            .unwrap();

        let events = desk.link.as_mut().unwrap().connection.drain();
        assert!(
            events.iter().any(|event| matches!(event,
                Event::TerminalAttached { terminal_id, error: Some(error), .. }
                    if terminal_id == &ResourceId::local(SELECTED)
                        && error.contains("fixture denies attach")
            )),
            "runtime translates the refusal into a terminal attach receipt"
        );
        // Match Desk::note_event ordering: cancel intent before Workspace
        // consumes the receipt, removes its awaiting entry, and queues notice.
        for event in &events {
            desk.note_fleet_event(event);
        }
        let link = desk.link.as_mut().unwrap();
        link.workspace.handle(&events, &link.connection);
        assert_eq!(link.workspace.pending(), 0);
        link.workspace.settle(&link.connection);
        desk.settle_fleet_terminal();

        assert!(desk.fleet_view.pending.is_none());
        let link = desk.link.as_mut().unwrap();
        assert!(take_selected_attaches(&link.connection).is_empty());
        let notices = link.workspace.take_notices();
        assert_eq!(notices.len(), 1, "the original Workspace notice survives");
        assert_eq!(notices[0].kind, "error");
        assert_eq!(notices[0].title, "Could not open terminal");
        assert!(
            notices[0]
                .body
                .as_deref()
                .unwrap()
                .contains("fixture denies attach")
        );

        settle(desk);
        desk.settle_fleet_terminal();
        let link = desk.link.as_mut().unwrap();
        assert!(
            take_selected_attaches(&link.connection).is_empty(),
            "later settlement must not issue a second AttachResource"
        );
        assert_eq!(link.workspace.pending(), 0);
        assert_eq!(link.workspace.views_of(&ResourceId::local(SELECTED)), 0);
        assert!(
            link.workspace.take_notices().is_empty(),
            "no repeated refusal"
        );
        assert_eq!(focused(desk), Some(ResourceId::local(DEFAULT)));
    });
}
