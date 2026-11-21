use super::*;
use phux_protocol::wire::frame::ErrorCode;

#[test]
fn close_terminals_terminates_each_under_organization() {
    let mut plane = seeded();
    let request = plane.close_terminals(vec![ResourceId::local(1)]).unwrap();
    let sent = commands(&mut plane);
    let [
        (
            id,
            Command::ResourceTerminate {
                operation, target, ..
            },
        ),
    ] = sent.as_slice()
    else {
        panic!("expected one terminate, got {sent:?}");
    };
    assert_eq!(target.resource, ResourceId::local(1));
    assert_eq!(target.activation, ActivationId::new(1));
    answer(
        &mut plane,
        *id,
        CommandResult::Accepted {
            operation: *operation,
        },
    );
    assert!(plane.take_events().iter().any(|e| matches!(
        e,
        Event::TerminalsClosed { request_id, error: None } if *request_id == request
    )));
}

#[test]
fn close_uses_the_legacy_batch_only_on_a_current_peer_that_advertises_it() {
    let ids = vec![ResourceId::local(1)];
    let (mut plane, _) = negotiate(ControlOptions::default(), server_hello().build(), None);
    assert!(plane.close_terminals(ids.clone()).is_none());
    assert!(commands(&mut plane).is_empty());

    let (mut plane, _) = negotiated(false);
    assert!(plane.close_terminals(Vec::new()).is_none());
    let request_id = plane.close_terminals(ids.clone()).expect("legacy support");
    assert_eq!(
        commands(&mut plane),
        [(request_id, Command::CloseTabResources { ids: ids.clone() })]
    );
    plane.connection_lost(None);
    assert!(
        plane.close_terminals(ids).is_none(),
        "cached HELLO is stale"
    );
    assert!(commands(&mut plane).is_empty());

    plane.connection_opened();
    assert!(plane.take_events().iter().any(|event| matches!(
        event,
        Event::TerminalsClosed { request_id: id, error: Some(error) }
            if *id == request_id && error.contains("outcome unknown")
    )));
}

#[test]
fn an_organization_peer_never_falls_back_while_seeding_or_missing_an_activation() {
    let (mut plane, seed) = negotiated(true);
    let ids = vec![ResourceId::local(1)];
    assert!(plane.close_terminals(ids.clone()).is_none());
    assert!(commands(&mut plane).is_empty());
    answer(
        &mut plane,
        seed.expect("seed"),
        page_value(page(5).with_resources(Vec::new())),
    );
    let _ = commands(&mut plane);

    let request_id = plane.close_terminals(ids).expect("correlated error");
    assert!(commands(&mut plane).is_empty());
    assert!(plane.take_events().iter().any(|event| matches!(
        event,
        Event::TerminalsClosed { request_id: id, error: Some(error) }
            if *id == request_id && error.contains("@1") && error.contains("activation")
    )));
}

#[test]
fn a_close_batch_can_be_partly_accepted_and_preserves_unknown_outcomes() {
    let mut plane = seeded();
    let read = plane.refresh_organization().unwrap();
    let _ = commands(&mut plane);
    let satellite = ResourceId::satellite("worker", 2);
    let inventory = page(6).with_resources(vec![
        running(1, skey(1)),
        running(2, skey(1)).with_live(Some(satellite.clone())),
    ]);
    answer(&mut plane, read, page_value(inventory));
    let _ = commands(&mut plane);

    let ids = vec![
        ResourceId::local(1),
        satellite.clone(),
        ResourceId::local(3),
    ];
    let close = plane.close_terminals(ids).unwrap();
    let sent = commands(&mut plane);
    let [
        (first, Command::ResourceTerminate { operation, .. }),
        (second, Command::ResourceTerminate { target, .. }),
    ] = sent.as_slice()
    else {
        panic!("expected two fenced terminates, got {sent:?}");
    };
    assert_eq!(target.resource, satellite);
    answer(
        &mut plane,
        *first,
        CommandResult::Accepted {
            operation: *operation,
        },
    );
    assert!(
        !plane
            .take_events()
            .iter()
            .any(|event| matches!(event, Event::TerminalsClosed { .. }))
    );
    answer(
        &mut plane,
        *second,
        CommandResult::error(ErrorCode::Unknown(0), "unreadable close reply"),
    );
    let events = plane.take_events();
    let errors: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::TerminalsClosed { request_id, error } if *request_id == close => {
                error.as_deref()
            }
            _ => None,
        })
        .collect();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("worker/@2: outcome unknown"));
    assert!(errors[0].contains("@3: the live activation is unavailable"));
    assert!(
        commands(&mut plane).is_empty(),
        "an unknown result is not resubmitted"
    );
}

#[test]
fn an_unmatched_acceptance_leaves_the_close_unknown_under_its_own_request() {
    let foreign = OperationId::new([254; 16]).unwrap();
    for reply in [
        CommandResult::Accepted { operation: foreign },
        CommandResult::Ok,
    ] {
        let mut plane = seeded();
        let close = plane.close_terminals(vec![ResourceId::local(1)]).unwrap();
        let sent = commands(&mut plane);
        let [(attempt, Command::ResourceTerminate { operation, .. })] = sent.as_slice() else {
            panic!("expected a fenced terminate, got {sent:?}");
        };
        assert_ne!(*operation, foreign);
        answer(&mut plane, *attempt, reply);
        assert!(plane.take_events().iter().any(|event| matches!(
            event,
            Event::TerminalsClosed { request_id, error: Some(error) }
                if *request_id == close && error.contains("@1: outcome unknown")
        )));
        assert!(commands(&mut plane).is_empty());
    }
}

#[test]
fn repeated_handles_send_one_terminate_in_first_occurrence_order() {
    let mut plane = seeded();
    let read = plane.refresh_organization().unwrap();
    let _ = commands(&mut plane);
    let local = ResourceId::local(1);
    let satellite = ResourceId::satellite("worker", 1);
    let inventory = page(6).with_resources(vec![
        running(1, skey(1)),
        running(2, skey(1)).with_live(Some(satellite.clone())),
    ]);
    answer(&mut plane, read, page_value(inventory));
    let _ = commands(&mut plane);

    plane
        .close_terminals(vec![
            satellite.clone(),
            local.clone(),
            satellite.clone(),
            local.clone(),
        ])
        .unwrap();
    let targets: Vec<_> = commands(&mut plane)
        .into_iter()
        .map(|(_, command)| match command {
            Command::ResourceTerminate { target, .. } => target.resource,
            other => panic!("expected a fenced terminate, got {other:?}"),
        })
        .collect();
    assert_eq!(targets, [satellite, local]);
}
