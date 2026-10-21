//! Routing commands to satellites, and post-negotiation frames.

use super::*;

#[test]
fn route_to_satellite_rewrites_terminal_ids_to_local() {
    let command = Command::GetScreen {
        terminal_id: ResourceId::satellite("devbox", 7),
        request_scrollback: Some(10),
        cells: true,
        format: 0,
    };
    let (routed_host, rewritten) = route_to_satellite(&command).expect("satellite target");
    assert_eq!(routed_host, host());
    assert_eq!(
        rewritten,
        Command::GetScreen {
            terminal_id: ResourceId::local(7),
            request_scrollback: Some(10),
            cells: true,
            format: 0,
        }
    );
}

#[test]
fn route_to_satellite_ignores_local_and_unscoped_commands() {
    assert!(
        route_to_satellite(&Command::GetScreen {
            terminal_id: ResourceId::local(7),
            request_scrollback: None,
            cells: false,
            format: 0,
        })
        .is_none()
    );
    assert!(
        route_to_satellite(&Command::GetState {
            scope: StateScope::Server,
        })
        .is_none()
    );
    assert!(route_to_satellite(&Command::Upgrade).is_none());
    assert!(
        route_to_satellite(&Command::KillResources {
            ids: vec![ResourceId::satellite("devbox", 1)],
            operation_id: None,
        })
        .is_none()
    );
    assert!(
        route_to_satellite(&Command::CloseTabResources {
            ids: vec![ResourceId::satellite("devbox", 1)],
        })
        .is_none()
    );
}

#[test]
fn route_to_satellite_covers_every_per_terminal_command() {
    let sat = ResourceId::satellite("devbox", 3);
    let commands = [
        Command::attach_terminal(sat.clone(), None),
        Command::DetachResource {
            terminal_id: sat.clone(),
        },
        Command::KillResource {
            terminal_id: sat.clone(),
            operation_id: None,
        },
        Command::GetTerminalState {
            terminal_id: sat.clone(),
            include_scrollback: false,
            max_scrollback_lines: 0,
        },
        Command::SubscribeResourceEvents {
            terminal_id: sat.clone(),
            event_types: vec![],
        },
        Command::AcquireInput {
            terminal_id: sat.clone(),
            mode: phux_protocol::wire::frame::InputMode::Cooperative,
            ttl_ms: 0,
        },
        Command::ReleaseInput {
            terminal_id: sat.clone(),
        },
        Command::SignalTerminal {
            terminal_id: sat.clone(),
            signal: phux_protocol::wire::frame::TerminalSignal::Interrupt,
            operation_id: None,
        },
        Command::ReportAsked {
            terminal_id: sat.clone(),
            id: "q".to_owned(),
            question: "?".to_owned(),
            suggestions: vec![],
            elapsed_seconds: None,
        },
        Command::ReportAgentState {
            terminal_id: sat.clone(),
            state: phux_protocol::wire::frame::ReportedAgentState::Done,
        },
        Command::ApplyInput {
            operation_id: phux_protocol::InputOperationId::new([1; 16]).expect("id"),
            terminal_id: sat.clone(),
            events: vec![],
        },
        Command::KillResourceIf {
            terminal_id: sat,
            precondition: phux_protocol::wire::frame::KillPrecondition::spawned_and_unattached(
                phux_protocol::ids::ServerInstance::new([3; 16]),
            ),
            operation_id: None,
        },
    ];
    for command in commands {
        let (routed_host, rewritten) =
            route_to_satellite(&command).expect("per-terminal command routes");
        assert_eq!(routed_host, host());
        assert!(
            route_to_satellite(&rewritten).is_none(),
            "rewritten command must be local: {rewritten:?}"
        );
    }
}

// --- session: command remap ------------------------------------------
