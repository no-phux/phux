use super::*;
use crate::control::ControlOptions;
use phux_protocol::ids::{
    AuthorityId, ResourceId, ResourceKey, ResourceKind, Revision, SessionId, SessionKey,
};
use phux_protocol::wire::frame::{OrgFilter, OrganizationPage, ResourceRecord, SessionRecord};

fn document_session(kind: ResourceKind, live: bool) -> ControlPlane {
    let mut plane = ControlPlane::new(ControlOptions::default());
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
    for kind in [
        ResourceKind::AccessibilityTree,
        ResourceKind::PixelSurface,
        ResourceKind::AgentSession,
    ] {
        let mut plane = document_session(kind, true);
        let request = plane.spawn_terminal_once(SpawnRequest::default());
        let outbound = plane.take_outbound();
        assert_eq!(outbound.len(), 1);
        let (frame, _) = FrameKind::decode(&outbound[0]).unwrap();
        assert!(
            matches!(frame, FrameKind::SpawnResource { request_id, owner_terminal: Some(owner), .. }
            if request_id == request && owner == ResourceId::local(17))
        );
    }
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
