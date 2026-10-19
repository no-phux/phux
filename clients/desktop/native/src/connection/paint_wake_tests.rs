use super::{admitted_terminal, paint_wake, session_attached};
use phux_client_runtime::control::{DeliveryOutcome, Event, Status};
use phux_protocol::ResourceId;

#[test]
fn output_and_delivered_input_skip_chrome() {
    let terminal_id = ResourceId::local(1);
    assert!(paint_wake(&Event::TerminalChanged {
        terminal_id: terminal_id.clone(),
    }));
    assert!(!Event::TerminalChanged { terminal_id }.changes_session_graph());
    assert!(paint_wake(&Event::InputDelivery {
        delivery_id: 1,
        outcome: DeliveryOutcome::Delivered,
        code: None,
        message: String::new(),
    }));
    assert!(!paint_wake(&Event::InputDelivery {
        delivery_id: 1,
        outcome: DeliveryOutcome::Refused,
        code: None,
        message: String::new(),
    }));
    assert!(Event::TopologyChanged.changes_session_graph());
    assert!(!paint_wake(&Event::TopologyChanged));
    assert!(session_attached(&Event::StatusChanged(Status::Attached)));
    assert!(!session_attached(&Event::StatusChanged(Status::Connecting)));
    assert!(!session_attached(&Event::TopologyChanged));
}

#[test]
fn only_successful_spawn_or_attach_is_admitted_before_inventory() {
    let terminal = ResourceId::local(7);
    assert_eq!(
        admitted_terminal(&Event::TerminalSpawned {
            request_id: 1,
            terminal_id: Some(terminal.clone()),
            error: None,
        }),
        Some(&terminal)
    );
    assert_eq!(
        admitted_terminal(&Event::TerminalSpawned {
            request_id: 2,
            terminal_id: Some(terminal.clone()),
            error: Some("refused".into()),
        }),
        None
    );
    assert_eq!(
        admitted_terminal(&Event::TerminalSpawned {
            request_id: 3,
            terminal_id: None,
            error: None,
        }),
        None
    );
    assert_eq!(
        admitted_terminal(&Event::TerminalAttached {
            request_id: 4,
            terminal_id: terminal.clone(),
            error: None
        }),
        Some(&terminal)
    );
    assert_eq!(
        admitted_terminal(&Event::TerminalAttached {
            request_id: 5,
            terminal_id: terminal,
            error: Some("refused".into())
        }),
        None
    );
    assert_eq!(admitted_terminal(&Event::TopologyChanged), None);
}
