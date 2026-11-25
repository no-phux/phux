use super::*;
use crate::control::ControlOptions;
use phux_protocol::ids::{AuthorityId, ResourceKey, ResourceKind, Revision, SessionKey};
use phux_protocol::wire::frame::{
    ArrangementDoc, ArrangementOwner, ArrangementValue, OrganizationPage, SessionRecord,
};

#[test]
fn external_placed_create_refreshes_the_cached_arrangement_after_the_resource_seed() {
    let mut control = ControlPlane::new(ControlOptions::default());
    let authority = AuthorityId::new([1; 16]).unwrap();
    let session = SessionKey::new([2; 16]).unwrap();
    let arrangement = ArrangementKey::new([3; 16]).unwrap();
    let revision = Revision::new(1);
    let page = |seq| {
        OrganizationPage::new(authority, revision, seq).with_sessions(vec![
            SessionRecord::new(session, "work", revision).with_arrangement(Some(arrangement)),
        ])
    };
    control.organization.begin_seed(OrgFilter::All);
    control.organization.apply_page(page(0));
    control
        .organization
        .apply_arrangement(ArrangementValue::new(
            arrangement,
            ArrangementOwner::Session(session),
            revision,
            ArrangementDoc::default(),
        ));

    control.organization_event(
        Some(1),
        &AgentEvent::ResourceCreated {
            resource: ResourceKey::new([4; 16]).unwrap(),
            session,
            kind: ResourceKind::Terminal,
        },
    );
    assert!(control.organization.seeding());
    control.take_outbound();
    control.organization_event(
        Some(2),
        &AgentEvent::ArrangementChanged {
            arrangement,
            owner: ArrangementOwner::Session(session),
            revision: Some(Revision::new(2)),
        },
    );
    control.resolve_org_read(
        &Pending::OrgPage(OrgFilter::Session(session)),
        CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page(2)))),
    );
    let outbound = control.take_outbound();
    assert_eq!(outbound.len(), 1);
    assert!(matches!(
        FrameKind::decode(&outbound[0]).unwrap().0,
        FrameKind::Command { command: Command::ArrangementGet { arrangement: key }, .. }
            if key == arrangement
    ));
}
