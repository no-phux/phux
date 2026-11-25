//! A spawn into a session with no live member creates the terminal by the
//! session's key instead of refusing.

use super::*;
use phux_client_runtime::control::SpawnRequest;
use phux_protocol::wire::frame::{CreatePlacement, LiveTarget};

fn hello_placed(placed: bool) -> FrameKind {
    let FrameKind::HelloOk {
        protocol_major,
        protocol_minor,
        protocol_patch,
        server_caps,
        server_id,
        selected_profile,
        bootstrap_limits,
    } = hello(true)
    else {
        unreachable!("hello is HELLO_OK");
    };
    let mut ext = vec![ServerFeatureExt::Organization];
    if placed {
        ext.push(ServerFeatureExt::CreatePlacement);
    }
    FrameKind::HelloOk {
        protocol_major,
        protocol_minor,
        protocol_patch,
        server_caps: server_caps.with_features_ext(ServerFeatureExtSet::with(&ext)),
        server_id,
        selected_profile,
        bootstrap_limits,
    }
}

/// A seeded plane whose session 2 (`skey(2)`) has no members.
fn with_empty_session(placed: bool) -> ControlPlane {
    let mut plane = opened(ControlOptions::default(), hello_placed(placed));
    let seed = commands(&mut plane)
        .into_iter()
        .find(|(_, c)| matches!(c, Command::GetOrganization { .. }))
        .expect("seed")
        .0;
    let page = page(5).with_sessions(vec![
        SessionRecord::new(skey(1), "work", Revision::new(2))
            .with_members(1)
            .with_live(Some(SessionId::new(1))),
        SessionRecord::new(skey(2), "empty", Revision::new(2)).with_live(Some(SessionId::new(2))),
    ]);
    answer(&mut plane, seed, page_value(page));
    assert!(plane.organization().is_live());
    let _ = commands(&mut plane);
    let _ = plane.take_events();
    plane
}

fn spawn_into_empty(plane: &mut ControlPlane) -> u32 {
    plane.spawn_terminal(SpawnRequest {
        session_id: Some(2),
        cwd: Some("/work".to_owned()),
        ..SpawnRequest::default()
    })
}

fn spawned(events: &[Event], request: u32) -> Option<(Option<ResourceId>, Option<String>)> {
    events.iter().find_map(|event| match event {
        Event::TerminalSpawned {
            request_id,
            terminal_id,
            error,
        } if *request_id == request => Some((terminal_id.clone(), error.clone())),
        _ => None,
    })
}

#[test]
fn an_empty_session_gets_a_terminal_created_by_its_key() {
    let mut plane = with_empty_session(true);
    let request = spawn_into_empty(&mut plane);
    let sent = commands(&mut plane);
    let [
        (
            attempt,
            Command::ResourceCreate {
                operation,
                session,
                launch,
                placement,
                ..
            },
        ),
    ] = sent.as_slice()
    else {
        panic!("expected one RESOURCE_CREATE, got {sent:?}");
    };
    assert_eq!(*session, skey(2));
    assert_eq!(launch.as_ref().unwrap().cwd.as_deref(), Some("/work"));
    assert!(matches!(
        placement.as_deref(),
        Some(CreatePlacement::NewGroup)
    ));
    assert!(
        spawned(&plane.take_events(), request).is_none(),
        "settles on the receipt"
    );

    let receipt = Committed::new(*operation, Revision::new(3))
        .with_resource(Some(rkey(9)))
        .with_target(Some(LiveTarget {
            resource: ResourceId::local(42),
            instance: instance(),
            activation: ActivationId::new(1),
        }));
    answer(
        &mut plane,
        *attempt,
        CommandResult::OkWith(CommandValue::Committed(Box::new(receipt))),
    );
    let events = plane.take_events();
    assert_eq!(
        spawned(&events, request),
        Some((Some(ResourceId::local(42)), None))
    );
    assert!(matches!(org_outcomes(&events).as_slice(),
        [(actual, OrgOutcome::Committed(_))] if actual == operation),
        "the canonical operation receipt remains visible alongside the spawn projection");
    assert!(
        commands(&mut plane).iter().any(|(_, c)| matches!(
            c,
            Command::AttachResource { terminal_id, .. } if *terminal_id == ResourceId::local(42)
        )),
        "the new terminal's output is subscribed"
    );
}

#[test]
fn a_refused_create_settles_the_spawn_with_its_reason() {
    let mut plane = with_empty_session(true);
    let request = spawn_into_empty(&mut plane);
    let (attempt, _) = commands(&mut plane).remove(0);
    answer(
        &mut plane,
        attempt,
        CommandResult::Refused(Refusal::NotFound),
    );
    let (terminal, error) = spawned(&plane.take_events(), request).expect("settled");
    assert!(terminal.is_none());
    assert!(error.is_some());
}

#[test]
fn a_server_without_create_placement_still_refuses_and_sends_nothing() {
    let mut plane = with_empty_session(false);
    let request = spawn_into_empty(&mut plane);
    assert!(commands(&mut plane).is_empty());
    let (terminal, error) = spawned(&plane.take_events(), request).expect("settled");
    assert!(terminal.is_none());
    assert!(error.unwrap().contains("selected session"));
}

#[test]
fn typed_spawn_into_empty_session_retains_both_operation_and_request() {
    use phux_client_runtime::control::WorkflowSubmission;
    let mut plane = with_empty_session(true);
    let WorkflowSubmission::Journaled { request_id: Some(request), operation } =
        plane.submit_spawn(SpawnRequest { session_id: Some(2), ..SpawnRequest::default() })
    else { panic!("empty-session creation must report its journal correlation"); };
    let sent = commands(&mut plane);
    let [(attempt, Command::ResourceCreate { operation: actual, .. })] = sent.as_slice()
    else { panic!("one exact create: {sent:?}"); };
    assert_eq!(*actual, operation);
    answer(&mut plane, *attempt, CommandResult::Refused(Refusal::NotFound));
    let events = plane.take_events();
    assert!(spawned(&events, request).is_some());
    assert!(matches!(org_outcomes(&events).as_slice(),
        [(actual, OrgOutcome::Refused(Refusal::NotFound))] if *actual == operation));
}
