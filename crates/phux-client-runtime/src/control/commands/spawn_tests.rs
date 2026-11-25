use super::*;
use crate::control::ControlOptions;
use phux_protocol::ids::{
    AuthorityId, ResourceId, ResourceKey, ResourceKind, Revision, SessionId, SessionKey,
};
use phux_protocol::wire::frame::{OrgFilter, OrganizationPage, ResourceRecord, SessionRecord};

fn document_session(kind: ResourceKind, live: bool) -> ControlPlane {
    let mut plane = ControlPlane::new(std::time::Instant::now(), ControlOptions::default());
    plane.attached_session = Some(1);
    plane.selected_session = Some(2);
    let session = SessionKey::new([2; 16]).unwrap();
    let record = SessionRecord::new(session, "documents", Revision::new(1))
        .with_live(Some(SessionId::new(2)));
    let resource = ResourceRecord::new(
        ResourceKey::new([3; 16]).unwrap(),
        session,
        Revision::new(1),
    )
    .with_kind(kind)
    .with_live(live.then(|| ResourceId::local(17)));
    let _ = plane.organization.begin_seed(OrgFilter::All);
    let page = OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(1), 0)
        .with_sessions(vec![record])
        .with_resources(vec![resource]);
    let _ = plane.organization.apply_page(page);
    assert!(plane.organization.is_live());
    plane
}

#[test]
fn foreign_document_only_session_routes_spawn_through_its_live_member() {
    let mut plane = document_session(ResourceKind::AgentSession, true);
    let request = plane.spawn_terminal_once(SpawnRequest::default());
    let outbound = plane.take_outbound();
    assert_eq!(outbound.len(), 1);
    let (frame, _) = FrameKind::decode(&outbound[0]).unwrap();
    assert!(
        matches!(frame, FrameKind::SpawnResource { request_id, owner_terminal: Some(owner), .. }
        if request_id == request && owner == ResourceId::local(17))
    );
}

#[test]
fn unresolved_foreign_session_refuses_without_spawning_in_home() {
    let mut plane = document_session(ResourceKind::PixelSurface, false);
    let request = plane.spawn_terminal_once(SpawnRequest::default());
    assert!(plane.take_outbound().is_empty());
    assert!(!plane.pending.contains_key(&request));
    assert!(plane.take_events().iter().any(|event| matches!(event,
        Event::TerminalSpawned { request_id, terminal_id: None, error: Some(error) }
            if *request_id == request && error.contains("selected session"))));
}

#[test]
fn explicit_session_wins_over_selection_and_home_needs_no_owner() {
    let mut plane = document_session(ResourceKind::PixelSurface, true);
    plane.spawn_terminal_once(SpawnRequest {
        session_id: Some(1),
        ..SpawnRequest::default()
    });
    let outbound = plane.take_outbound();
    assert!(matches!(
        FrameKind::decode(&outbound[0]).unwrap().0,
        FrameKind::SpawnResource {
            owner_terminal: None,
            ..
        }
    ));
}

#[test]
fn typed_unavailable_spawn_leaves_no_queue_journal_or_reply() {
    let mut plane = ControlPlane::new(std::time::Instant::now(), ControlOptions::default());
    assert!(matches!(plane.submit_spawn(SpawnRequest::default()), WorkflowSubmission::NotQueued { .. }));
    assert!(plane.take_outbound().is_empty());
    assert!(plane.take_events().is_empty());
    assert!(plane.pending_organization_operations().next().is_none());
}

#[test]
fn lost_once_only_spawn_receipt_is_typed_unknown_and_never_replayable() {
    let mut plane = document_session(ResourceKind::Terminal, true);
    plane.handshake_ready = true;
    plane.status = Status::Attached;
    let WorkflowSubmission::Queued { request_id } = plane.submit_spawn_once(SpawnRequest::default())
    else { panic!("once-only spawn must queue"); };
    let _ = plane.take_outbound();
    plane.connection_lost(Some("receipt lost".into()));
    assert!(plane.take_events().iter().any(|event| matches!(event,
        Event::TerminalCreation { request_id: actual,
            outcome: crate::control::TerminalCreationOutcome::Unknown { .. } }
            if *actual == request_id)));
    assert!(!plane.pending.contains_key(&request_id));
    assert!(plane.take_outbound().is_empty(), "unknown is never permission to launch another terminal");
}
