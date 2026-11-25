//! Native actions use host-owned create/ensure and retain original uncertainty.
use super::*;
use phux_client_core::org_ops::OrgOutcome;
use phux_client_runtime::control::{ControlPlane, Event};
use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
use phux_protocol::ids::{AuthorityId, OperationId, Revision};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, OrganizationPage, Refusal,
};
use std::sync::Arc;

fn window() -> (Arc<Client>, Desk) {
    let client = Arc::new(Runtime::embedded(ControlOptions::default()));
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(
            phux_wire_fixtures::HelloOk::new([0xAB; 16], phux_wire_fixtures::l3_caps())
                .features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
                .features_ext2(phux_protocol::caps::ServerFeatureExt2Set::with(&[
                    phux_protocol::caps::ServerFeatureExt2::SessionEnsure,
                ]))
                .build(),
        )
        .expect("HELLO_OK");
    // The handshake's own follow-ups (the event subscription, the inventory
    // read) are connection work, not workflow work; only find the seed.
    let request =
        crate::wire_fixture::organization_request(&crate::wire_fixture::outbound(&client))
            .expect("seed");
    client
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(
                OrganizationPage::new(
                    AuthorityId::new([1; 16]).expect("authority"),
                    Revision::new(1),
                    0,
                ),
            ))),
        })
        .expect("seed response");
    let _ = client.take_events();
    let _ = client.take_outbound();
    let mut desk = crate::test_support::window();
    desk.connection.client = Some(Arc::clone(&client));
    desk.connection.status = Status::Negotiated;
    (client, desk)
}

fn sent(client: &Client) -> Vec<(u32, Command)> {
    client
        .take_outbound()
        .iter()
        .map(|bytes| match FrameKind::decode(bytes).expect("frame").0 {
            FrameKind::Command {
                request_id,
                command,
            } => (request_id, command),
            other => panic!("unexpected non-command workflow work: {other:?}"),
        })
        .collect()
}

fn settle(desk: &mut Desk, client: &Client) {
    for event in client.take_events() {
        if let Event::OrganizationOperation { operation, outcome } = event {
            assert!(desk.session_creation_outcome(client, operation, &outcome));
        }
    }
}

#[test]
fn native_new_session_is_exact_create_and_a_collision_does_not_attach() {
    let (client, mut desk) = window();
    desk.create_session("collision".into());
    let outgoing = sent(&client);
    let [
        (
            request,
            Command::SessionCreate {
                operation,
                lifetime,
                ..
            },
        ),
    ] = outgoing.as_slice()
    else {
        panic!("exact create: {outgoing:?}");
    };
    assert_eq!(
        *lifetime,
        phux_protocol::wire::frame::SessionLifetime::Persistent
    );
    assert_eq!(desk.link.creating, Some(*operation));
    client
        .feed(FrameKind::CommandResult {
            request_id: *request,
            result: CommandResult::Refused(Refusal::NameTaken),
        })
        .expect("collision receipt");
    settle(&mut desk, &client);
    assert!(desk.link.creating.is_none());
    assert!(desk.connection.pending_session.is_none());
    assert!(desk.connection.error.is_some());
    assert!(
        sent(&client).is_empty(),
        "collision must not select or attach the existing session"
    );
}

#[test]
fn native_home_ensure_is_not_a_local_existence_create_and_unknown_blocks_automatic_retry() {
    let (client, mut desk) = window();
    desk.ensure_session("work".into());
    let outgoing = sent(&client);
    let [(_, Command::SessionEnsure { operation, .. })] = outgoing.as_slice() else {
        panic!("host ensure: {outgoing:?}");
    };
    let original: OperationId = *operation;
    assert!(desk.session_creation_outcome(
        &client,
        original,
        &OrgOutcome::Unknown {
            reason: "receipt expired".into()
        }
    ));
    assert_eq!(desk.link.uncertain_session, Some(original));
    desk.connection.topology = Some(crate::desk::topology(Vec::new(), Vec::new()));
    desk.launch.session = Some("work".into());
    desk.connection.status = Status::Attached;
    assert!(!desk.settle_home(&client));
    assert!(
        sent(&client).is_empty(),
        "unknown never authorizes a fresh ensure/create"
    );
}
