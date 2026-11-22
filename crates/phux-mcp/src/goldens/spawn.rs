use phux_client::testkit::{OrgFixture, ScriptSpec};
use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
use phux_protocol::ids::{
    ActivationId, AuthorityId, OperationId, ResourceId, ResourceKey, Revision,
};
use phux_protocol::wire::frame::{
    ArrangementDoc, ArrangementGroup, ArrangementNode, CommandResult, CommandValue, Committed,
    EntryRef, EntryTarget, LiveTarget, SessionLifetime,
};

pub(super) fn placed_spec() -> ScriptSpec {
    let mut fixture = OrgFixture::new();
    let session = fixture.session("work", SessionLifetime::Persistent);
    let anchor = fixture.shell(session);
    assert_eq!(fixture.live(&anchor), Some(ResourceId::local(4)));
    let arrangement = fixture.arrangement(
        session,
        ArrangementDoc {
            groups: vec![ArrangementGroup::new(
                [1; 16],
                None,
                ArrangementNode::Leaf(EntryRef::new([2; 16], EntryTarget::Member(anchor))),
            )],
        },
    );
    let instance = fixture.instance();
    super::org_spec(fixture)
        .server_features_ext(ServerFeatureExtSet::with(&[
            ServerFeatureExt::Organization,
            ServerFeatureExt::CreatePlacement,
        ]))
        .mutation_reply(CommandResult::OkWith(CommandValue::Committed(Box::new(
            Committed::new(OperationId::new([7; 16]).unwrap(), Revision::new(6))
                .with_resource(ResourceKey::new([4; 16]))
                .with_creation(
                    AuthorityId::new([0xa1; 16]),
                    Some(arrangement),
                    Some([3; 16]),
                    None,
                )
                .with_target(Some(LiveTarget {
                    resource: ResourceId::local(7),
                    instance,
                    activation: ActivationId::new(1),
                })),
        ))))
}
