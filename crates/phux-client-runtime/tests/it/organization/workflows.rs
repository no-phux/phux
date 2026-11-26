//! Consumer workflow entry points retain exact operation identity and intent.
use super::*;
use phux_client_runtime::control::WorkflowSubmission;
use phux_protocol::wire::frame::SessionLifetime;

fn create(plane: &mut ControlPlane, name: &str) -> OperationId {
    let WorkflowSubmission::Journaled {
        request_id: None,
        operation,
    } = plane.create_session(name.into(), SessionLifetime::Persistent, None)
    else {
        panic!("exact creation must retain a journal operation");
    };
    operation
}

#[test]
fn create_collision_is_not_an_attach_or_an_ensure() {
    let mut plane = seeded();
    let selected = plane.selected_session();
    let operation = create(&mut plane, "work");
    let sent = commands(&mut plane);
    let [
        (
            request_id,
            Command::SessionCreate {
                operation: actual,
                name,
                ..
            },
        ),
    ] = sent.as_slice()
    else {
        panic!("one exact create, not an attach: {sent:?}");
    };
    assert_eq!(*actual, operation);
    assert_eq!(name, "work");
    answer(
        &mut plane,
        *request_id,
        CommandResult::Refused(Refusal::NameTaken),
    );
    assert!(plane.take_events().iter().any(|event| matches!(event,
        Event::OrganizationOperation { operation: actual, outcome: OrgOutcome::Refused(Refusal::NameTaken) }
            if *actual == operation)));
    assert_eq!(plane.selected_session(), selected);
    assert!(commands(&mut plane).is_empty());
}

#[test]
fn lost_create_receipt_reconciles_original_operation_then_reports_unknown_at_another_authority() {
    let mut plane = seeded();
    let operation = create(&mut plane, "new");
    let _ = commands(&mut plane);
    let (seed, before_seed) = reconnect(&mut plane);
    assert!(
        !before_seed
            .iter()
            .any(|(_, command)| matches!(command, Command::SessionCreate { .. }))
    );
    answer(&mut plane, seed, page_value(page(8)));
    let resent = commands(&mut plane);
    assert!(
        matches!(resent.as_slice(), [(_, Command::SessionCreate { operation: actual, .. })] if *actual == operation)
    );
    let (seed, _) = reconnect(&mut plane);
    answer(
        &mut plane,
        seed,
        page_value(OrganizationPage::new(
            AuthorityId::new([2; 16]).unwrap(),
            Revision::new(1),
            0,
        )),
    );
    assert!(plane.take_events().iter().any(|event| matches!(event,
        Event::OrganizationOperation { operation: actual, outcome: OrgOutcome::Unknown { .. } }
            if *actual == operation)));
    assert!(plane.pending_organization_operations().next().is_none());
    assert!(
        !commands(&mut plane)
            .iter()
            .any(|(_, command)| matches!(command, Command::SessionCreate { .. }))
    );
}

#[test]
fn unavailable_create_accepts_no_work_for_a_later_peer() {
    let (mut plane, _) = negotiated(false);
    let _ = commands(&mut plane);
    assert!(matches!(
        plane.create_session("new".into(), SessionLifetime::Persistent, None),
        WorkflowSubmission::NotQueued { .. }
    ));
    assert!(plane.pending_organization_operations().next().is_none());
    assert!(commands(&mut plane).is_empty());
}

fn ensuring() -> ControlPlane {
    let hello = server_hello()
        .features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
        .features_ext2(phux_protocol::caps::ServerFeatureExt2Set::with(&[
            phux_protocol::caps::ServerFeatureExt2::SessionEnsure,
        ]))
        .build();
    let mut plane = opened(ControlOptions::default(), hello);
    let seed = commands(&mut plane)
        .into_iter()
        .find_map(|(request, command)| {
            matches!(command, Command::GetOrganization { .. }).then_some(request)
        })
        .expect("seed");
    answer(&mut plane, seed, page_value(page(5)));
    let _ = commands(&mut plane);
    plane
}

#[test]
fn ensure_is_host_resolved_and_preserves_exact_existing_or_created_receipt() {
    for created in [false, true] {
        let mut plane = ensuring();
        let WorkflowSubmission::Journaled {
            request_id: None,
            operation,
        } = plane.ensure_session("work".into(), SessionLifetime::Persistent, None)
        else {
            panic!("supported ensure must be journaled");
        };
        let sent = commands(&mut plane);
        let [
            (
                request,
                Command::SessionEnsure {
                    operation: actual,
                    name,
                    ..
                },
            ),
        ] = sent.as_slice()
        else {
            panic!("ensure is not a local lookup or create: {sent:?}");
        };
        assert_eq!(*actual, operation);
        assert_eq!(name, "work");
        let mut receipt = Committed::new(operation, Revision::new(11)).with_session(Some(skey(1)));
        receipt.ensured_created = Some(created);
        answer(
            &mut plane,
            *request,
            CommandResult::OkWith(CommandValue::Committed(Box::new(receipt))),
        );
        assert!(plane.take_events().iter().any(|event| matches!(event,
            Event::OrganizationOperation { operation: actual, outcome: OrgOutcome::Committed(receipt) }
                if *actual == operation && receipt.session == Some(skey(1)) && receipt.ensured_created == Some(created))));
        assert!(
            commands(&mut plane).is_empty(),
            "ensuring never implicitly attaches or adds a terminal"
        );
        assert_eq!(plane.selected_session(), None);
    }
}

#[test]
fn older_peer_ensure_is_unsupported_without_hidden_create() {
    let mut plane = seeded();
    assert!(
        matches!(plane.ensure_session("missing".into(), SessionLifetime::Persistent, None),
        WorkflowSubmission::NotQueued { reason } if reason == "ensure_session_unsupported")
    );
    assert!(plane.pending_organization_operations().next().is_none());
    assert!(commands(&mut plane).is_empty());
}
