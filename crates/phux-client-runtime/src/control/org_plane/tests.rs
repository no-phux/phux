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
    let sibling = SessionKey::new([5; 16]).unwrap();
    let sibling_arrangement = ArrangementKey::new([6; 16]).unwrap();
    let revision = Revision::new(1);
    let page = |seq| {
        OrganizationPage::new(authority, revision, seq).with_sessions(vec![
            SessionRecord::new(session, "work", revision).with_arrangement(Some(arrangement)),
        ])
    };
    control.organization.begin_seed(OrgFilter::All);
    let mut initial = page(0);
    initial.sessions.push(
        SessionRecord::new(sibling, "other", revision).with_arrangement(Some(sibling_arrangement)),
    );
    control.organization.apply_page(initial);
    control
        .organization
        .apply_arrangement(ArrangementValue::new(
            arrangement,
            ArrangementOwner::Session(session),
            revision,
            ArrangementDoc::default(),
        ));
    control
        .organization
        .apply_arrangement(ArrangementValue::new(
            sibling_arrangement,
            ArrangementOwner::Session(sibling),
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
    control.organization_event(
        Some(3),
        &AgentEvent::ArrangementChanged {
            arrangement: sibling_arrangement,
            owner: ArrangementOwner::Session(sibling),
            revision: Some(Revision::new(2)),
        },
    );
    control.resolve_org_read(
        &Pending::OrgPage(OrgFilter::Session(session)),
        CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page(2)))),
    );
    let keys: HashSet<_> = control
        .take_outbound()
        .into_iter()
        .map(|bytes| match FrameKind::decode(&bytes).unwrap().0 {
            FrameKind::Command {
                command: Command::ArrangementGet { arrangement },
                ..
            } => arrangement,
            other => panic!("unexpected frame: {other:?}"),
        })
        .collect();
    assert_eq!(keys, HashSet::from([arrangement, sibling_arrangement]));
}
