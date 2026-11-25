#![allow(clippy::expect_used, clippy::panic, reason = "test assertions")]

use super::*;
use phux_client_runtime::control::{ControlOptions, ControlPlane};
use phux_protocol::ids::{AuthorityId, Revision, SessionId, WindowId};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, FrameKind, OrganizationPage};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
const COLS: u16 = 24;
const ROWS: u16 = 4;
#[path = "../search/tests/support.rs"]
mod support;
use support::remote_with;
fn bootstrap(client: &Client, terminal_id: ResourceId, bytes: &[u8]) {
    for frame in phux_wire_fixtures::bootstrap_frames(&terminal_id, [1, 1], [COLS, ROWS], bytes) {
        client.feed(frame).expect("bootstrap frame");
    }
}

fn negotiate(client: &Client, authority: u8) {
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client.feed(phux_wire_fixtures::HelloOk::new([0xAB; 16], phux_wire_fixtures::l3_caps())
        .features_ext(phux_protocol::caps::ServerFeatureExtSet::with(&[
            phux_protocol::caps::ServerFeatureExt::Organization,
        ]))
        .features_ext2(phux_protocol::caps::ServerFeatureExt2Set::with(&[phux_protocol::caps::ServerFeatureExt2::SessionEnsure]))
        .build()).expect("HELLO_OK");
    let before_seed = client.take_outbound();
    assert!(!before_seed.iter().any(|bytes| matches!(phux_wire_fixtures::decode_one(bytes),
        FrameKind::Command { command: Command::SessionCreate { .. }, .. })),
        "never resend before authority reconciliation");
    let request_id = before_seed.iter().find_map(|bytes| match phux_wire_fixtures::decode_one(bytes) {
        FrameKind::Command { request_id, command: Command::GetOrganization { .. } } => Some(request_id),
        _ => None,
    }).expect("organization seed");
    client.feed(FrameKind::CommandResult { request_id, result: CommandResult::OkWith(
        CommandValue::OrganizationPage(Box::new(OrganizationPage::new(
            AuthorityId::new([authority; 16]).expect("authority"), Revision::new(1), 0,
        ))),
    ) }).expect("seed response");
    assert!(!client.take_outbound().iter().any(|bytes| matches!(phux_wire_fixtures::decode_one(bytes),
        FrameKind::Command { command: Command::SessionCreate { .. }, .. })),
        "no new create at another authority");
}

#[test]
fn unavailable_spawn_has_no_hidden_ensure_or_selection() {
    let remote = remote_with(&[(1, b"original".to_vec())]);
    let client = remote.runtime_client().expect("connected");
    let selected = client.selected_session();
    assert!(matches!(remote.spawn_terminal_in_directory("missing".into(), Some("/tmp".into())),
        WireWorkflowSubmission::NotQueued { .. }));
    assert!(client.take_outbound().is_empty());
    assert_eq!(client.selected_session(), selected);
    let WireWorkflowSubmission::Queued { request_id } = remote.spawn_terminal_in("main".into()) else {
        panic!("existing-session creation must retain request correlation");
    };
    assert!(client.take_outbound().iter().any(|bytes| matches!(phux_wire_fixtures::decode_one(bytes),
        FrameKind::SpawnResource { request_id: actual, .. } if actual == request_id)));
    assert_eq!(client.selected_session(), selected);
}

#[test]
fn foreign_create_retains_original_operation_and_reports_unknown_without_recreating() {
    let client = Runtime::embedded(ControlOptions::default());
    negotiate(&client, 1);
    let remote = RemoteClient::new("unused".into(), COLS, ROWS, None, None);
    *remote.client.lock().expect("client slot") = Some(client.clone());
    let operation = remote.create_session("new".into(), organization::WireSessionLifetime::Persistent, None)
        .expect("journaled create");
    let creates: Vec<_> = client.take_outbound().iter().filter_map(|bytes| match phux_wire_fixtures::decode_one(bytes) {
        FrameKind::Command { command: Command::SessionCreate { operation, name, .. }, .. } => Some((operation, name)),
        _ => None,
    }).collect();
    assert_eq!(creates.len(), 1);
    assert_eq!(organization::operation_text(&creates[0].0), operation);
    client.with_control(|control| control.connection_lost(Some("receipt lost".into())));
    negotiate(&client, 2);
    let publication = remote.take_publication();
    assert!(publication.events.iter().any(|event| matches!(event,
        WireEvent::OrganizationOperation { operation: actual, status, .. }
            if actual == &operation && status == "unknown")));
    assert!(client.with_control(|control| control.pending_organization_operations().next().is_none()));
    assert!(!client.take_outbound().iter().any(|bytes| matches!(phux_wire_fixtures::decode_one(bytes),
        FrameKind::Command { command: Command::SessionCreate { .. }, .. })));
}

#[test]
fn foreign_ensure_retains_exact_existing_and_created_session_outcomes() {
    let client = Runtime::embedded(ControlOptions::default());
    negotiate(&client, 1);
    let remote = RemoteClient::new("unused".into(), COLS, ROWS, None, None);
    *remote.client.lock().expect("client slot") = Some(client.clone());
    for created in [false, true] {
        let WireWorkflowSubmission::Journaled { request_id: None, operation } =
            remote.ensure_session("work".into(), organization::WireSessionLifetime::Persistent, None)
        else { panic!("ensure must expose the retained operation"); };
        let (request, actual) = client.take_outbound().iter().find_map(|bytes| match phux_wire_fixtures::decode_one(bytes) {
            FrameKind::Command { request_id, command: Command::SessionEnsure { operation, .. } } => Some((request_id, operation)),
            _ => None,
        }).expect("one host ensure");
        assert_eq!(organization::operation_text(&actual), operation);
        let session = phux_protocol::ids::SessionKey::new([7; 16]).expect("session");
        let mut receipt = phux_protocol::wire::frame::Committed::new(actual, Revision::new(2)).with_session(Some(session));
        receipt.ensured_created = Some(created);
        client.feed(FrameKind::CommandResult { request_id: request,
            result: CommandResult::OkWith(CommandValue::Committed(Box::new(receipt))) }).expect("ensure receipt");
        assert!(remote.take_events().iter().any(|event| matches!(event,
            WireEvent::OrganizationOperation { operation: actual, created_session_key, ensured_created, .. }
                if actual == &operation && created_session_key.as_deref() == Some(session.to_string().as_str())
                    && *ensured_created == Some(created))));
        assert!(client.take_outbound().is_empty(), "ensure does not select or spawn afterward");
    }
}

#[test]
fn queued_foreign_spawn_settles_unknown_distinctly_from_refusal() {
    for uncertain in [false, true] {
        let remote = remote_with(&[(1, b"original".to_vec())]);
        let client = remote.runtime_client().expect("connected");
        let WireWorkflowSubmission::Queued { request_id } = remote.spawn_terminal() else {
            panic!("queued spawn must retain request identity");
        };
        let _ = client.take_outbound();
        client.feed(FrameKind::Error { request_id: Some(request_id),
            code: if uncertain { phux_protocol::wire::frame::ErrorCode::UNREADABLE_REPLY }
                else { phux_protocol::wire::frame::ErrorCode::Unsupported },
            message: "peer reply".into(),
        }).expect("settlement");
        let events = remote.take_events();
        assert!(events.iter().any(|event| matches!(event,
            WireEvent::TerminalCreation { request_id: actual, outcome }
                if *actual == request_id && matches!((uncertain, outcome),
                    (true, WireTerminalCreationOutcome::Unknown { .. })
                    | (false, WireTerminalCreationOutcome::Refused { .. })))));
        assert!(!client.take_outbound().iter().any(|bytes| matches!(
            phux_wire_fixtures::decode_one(bytes), FrameKind::SpawnResource { .. })),
            "unknown receipt never creates a replacement");
    }
}

#[test]
fn foreign_missing_or_mismatched_session_receipt_stays_unknown_under_original_operation() {
    for malformed in 0..3 {
        let client = Runtime::embedded(ControlOptions::default());
        negotiate(&client, 1);
        let remote = RemoteClient::new("unused".into(), COLS, ROWS, None, None);
        *remote.client.lock().expect("client slot") = Some(client.clone());
        let original = if malformed == 0 {
            remote.create_session("work".into(), organization::WireSessionLifetime::Persistent, None).expect("journaled create")
        } else {
            let WireWorkflowSubmission::Journaled { operation, .. } =
                remote.ensure_session("work".into(), organization::WireSessionLifetime::Persistent, None)
            else { panic!("journaled ensure"); };
            operation
        };
        let (request_id, operation) = client.take_outbound().iter().find_map(|bytes| match phux_wire_fixtures::decode_one(bytes) {
            FrameKind::Command { request_id, command: Command::SessionCreate { operation, .. }
                | Command::SessionEnsure { operation, .. } } => Some((request_id, operation)),
            _ => None,
        }).expect("original creation request");
        let acknowledged = if malformed == 2 {
            phux_protocol::ids::OperationId::new([0x55; 16]).expect("foreign operation")
        } else { operation };
        let mut receipt = phux_protocol::wire::frame::Committed::new(acknowledged, Revision::new(2));
        if malformed != 0 {
            receipt.session = phux_protocol::ids::SessionKey::new([7; 16]);
        }
        if malformed == 2 { receipt.ensured_created = Some(false); }
        client.feed(FrameKind::CommandResult { request_id,
            result: CommandResult::OkWith(CommandValue::Committed(Box::new(receipt))) }).expect("malformed receipt");
        assert!(remote.take_events().iter().any(|event| matches!(event,
            WireEvent::OrganizationOperation { operation, status, .. }
                if operation == &original && status == "unknown")));
        assert!(client.with_control(|control| control.pending_organization_operations().next().is_none()));
        assert!(!client.take_outbound().iter().any(|bytes| matches!(phux_wire_fixtures::decode_one(bytes),
            FrameKind::Attach { .. } | FrameKind::SpawnResource { .. } | FrameKind::Command {
                command: Command::SessionCreate { .. } | Command::SessionEnsure { .. }, .. })),
            "unknown cannot select or create under another identity");
    }
}
