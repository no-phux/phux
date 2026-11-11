//! Additive diagnostic/persistence fields and exact-key resource filtering.
#![allow(clippy::unwrap_used)]
use bytes::BytesMut;
use phux_protocol::{
    api::{LaunchDiagnostic, LaunchStage},
    ids::*,
    wire::frame::*,
};

fn roundtrip(frame: &FrameKind) {
    let mut bytes = BytesMut::new();
    frame.encode(&mut bytes);
    let (decoded, rest) = FrameKind::decode(&bytes).unwrap();
    assert!(rest.is_empty());
    assert_eq!(&decoded, frame);
}

#[test]
fn durable_resource_filter_roundtrips_in_existing_organization_command() {
    roundtrip(&FrameKind::Command {
        request_id: 1,
        command: Command::GetOrganization {
            filter: OrgFilter::Resource(ResourceKey::new([9; 16]).unwrap()),
            page_size: 2,
            cursor: None,
        },
    });
}
#[test]
fn inventory_and_creation_carry_identical_optional_evidence() {
    let activation = ActivationInfo::new(
        ActivationId::new(1),
        ServerInstance::new([4; 16]),
        ExecState::Failed,
    )
    .with_outcome(Some(Outcome::Unobserved))
    .with_evidence(
        Some(LaunchDiagnostic {
            stage: LaunchStage::Exec,
            errno: Some(2),
        }),
        true,
    );
    let resource = ResourceKey::new([3; 16]).unwrap();
    let session = SessionKey::new([2; 16]).unwrap();
    let row =
        ResourceRecord::new(resource, session, Revision::new(2)).with_activation(Some(activation));
    let page = OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(2), 2)
        .with_resources(vec![row]);
    roundtrip(&FrameKind::CommandResult {
        request_id: 1,
        result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
    });
    let receipt = Committed::new(OperationId::new([5; 16]).unwrap(), Revision::new(2))
        .with_creation(None, None, None, Some(activation));
    roundtrip(&FrameKind::CommandResult {
        request_id: 2,
        result: CommandResult::OkWith(CommandValue::Committed(Box::new(receipt))),
    });
}
