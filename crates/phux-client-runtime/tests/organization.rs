//! The control plane's organization lane driven sans-IO: frames fed
//! directly, outbound commands answered by hand.

#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::unwrap_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]

use phux_client_core::org_ops::{OrgOutcome, SessionOutcome, TargetOutcome};
use phux_client_runtime::control::{ControlOptions, ControlPlane, Event, ReplicaState};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, Layer, LayerSet, ServerCapabilities, ServerFeature,
    ServerFeatureExt, ServerFeatureExtSet, ServerFeatureSet,
};
use phux_protocol::ids::{
    ActivationId, AuthorityId, OperationId, ResourceId, ResourceKey, Revision, ServerInstance,
    SessionId, SessionKey, WindowId,
};
use phux_protocol::wire::frame::{
    ActivationInfo, AgentEvent, Command, CommandResult, CommandValue, Committed, EventStamp,
    ExecState, FrameKind, OrgFilter, OrganizationPage, Outcome, Refusal, ResourceRecord,
    SessionRecord,
};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot};

fn hello(organization: bool) -> FrameKind {
    let ext = if organization {
        ServerFeatureExtSet::with(&[ServerFeatureExt::Organization])
    } else {
        ServerFeatureExtSet::new()
    };
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new()
            .with_layers(LayerSet::with(&[Layer::L3]))
            .with_features(ServerFeatureSet::with(&[ServerFeature::CloseTabResources]))
            .with_features_ext(ext),
        server_id: vec![0xAB; 16],
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    }
}

fn decode(frame: &[u8]) -> FrameKind {
    let (decoded, rest) = FrameKind::decode(frame).expect("outbound frame decodes");
    assert!(rest.is_empty());
    decoded
}

/// Every outbound `COMMAND`, with its request id.
fn commands(plane: &mut ControlPlane) -> Vec<(u32, Command)> {
    plane
        .take_outbound()
        .iter()
        .filter_map(|bytes| match decode(bytes) {
            FrameKind::Command {
                request_id,
                command,
            } => Some((request_id, command)),
            _ => None,
        })
        .collect()
}

fn answer(plane: &mut ControlPlane, request_id: u32, result: CommandResult) {
    plane
        .feed(FrameKind::CommandResult { request_id, result })
        .expect("command result");
}

fn event(plane: &mut ControlPlane, seq: Option<u64>, event: AgentEvent) {
    plane
        .feed(FrameKind::Event {
            terminal: None,
            event,
            stamp: seq.map(|seq| Box::new(EventStamp::new(seq, 0))),
        })
        .expect("event");
}

const fn skey(n: u8) -> SessionKey {
    SessionKey::new([n; 16]).unwrap()
}

const fn rkey(n: u8) -> ResourceKey {
    ResourceKey::new([0x40 | n; 16]).unwrap()
}

const fn instance() -> ServerInstance {
    ServerInstance::new([9; 16])
}

fn running(n: u8, session: SessionKey) -> ResourceRecord {
    ResourceRecord::new(rkey(n), session, Revision::new(1))
        .with_activation(Some(ActivationInfo::new(
            ActivationId::new(1),
            instance(),
            ExecState::Running,
        )))
        .with_live(Some(ResourceId::local(u32::from(n))))
}

fn page(seq: u64) -> OrganizationPage {
    OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(10), seq)
        .with_sessions(vec![
            SessionRecord::new(skey(1), "work", Revision::new(2))
                .with_members(1)
                .with_live(Some(SessionId::new(1))),
        ])
        .with_resources(vec![running(1, skey(1))])
}

fn page_value(page: OrganizationPage) -> CommandResult {
    CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page)))
}

fn committed(operation: OperationId, revision: u64) -> CommandResult {
    CommandResult::OkWith(CommandValue::Committed(Box::new(Committed::new(
        operation,
        Revision::new(revision),
    ))))
}

/// A browsing plane past `HELLO_OK`; returns the seed request's id when the
/// server advertises `ORGANIZATION`.
fn negotiated(organization: bool) -> (ControlPlane, Option<u32>) {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(hello(organization)).expect("HELLO_OK");
    let seed = commands(&mut plane)
        .into_iter()
        .find(|(_, command)| {
            matches!(
                command,
                Command::GetOrganization {
                    filter: OrgFilter::All,
                    cursor: None,
                    ..
                }
            )
        })
        .map(|(id, _)| id);
    (plane, seed)
}

fn seeded() -> ControlPlane {
    let (mut plane, seed) = negotiated(true);
    answer(&mut plane, seed.expect("seed"), page_value(page(5)));
    plane
}

fn org_outcomes(events: &[Event]) -> Vec<(OperationId, OrgOutcome)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::OrganizationOperation { operation, outcome } => {
                Some((*operation, outcome.clone()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn the_replica_seeds_across_pages_then_follows_the_journal() {
    let (mut plane, seed) = negotiated(true);
    let seed = seed.expect("an ORGANIZATION server is seeded after HELLO_OK");
    assert_eq!(plane.organization().state(), ReplicaState::Seeding);
    let first = OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(10), 5)
        .with_sessions(vec![SessionRecord::new(skey(1), "work", Revision::new(2))])
        .with_next_cursor(Some(vec![7]));
    answer(&mut plane, seed, page_value(first));
    let next = commands(&mut plane)
        .into_iter()
        .find(|(_, c)| {
            matches!(
                c,
                Command::GetOrganization {
                    cursor: Some(_),
                    ..
                }
            )
        })
        .expect("the next page")
        .0;
    // An event during the seed is buffered and replayed past the cut.
    event(
        &mut plane,
        Some(6),
        AgentEvent::SessionRenamed {
            session: skey(1),
            name: "notes".to_owned(),
            revision: Revision::new(3),
        },
    );
    answer(
        &mut plane,
        next,
        page_value(
            OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(10), 5)
                .with_resources(vec![running(1, skey(1))]),
        ),
    );
    assert!(plane.organization().is_live());
    assert_eq!(
        plane.organization().session(&skey(1)).unwrap().name,
        "notes"
    );
    assert_eq!(
        plane.organization().handles().handle(&rkey(1)),
        Some(&ResourceId::local(1))
    );
    assert!(
        plane
            .take_events()
            .iter()
            .any(|e| matches!(e, Event::OrganizationChanged))
    );
    event(
        &mut plane,
        Some(7),
        AgentEvent::ActivationChanged {
            resource: rkey(1),
            activation: ActivationId::new(1),
            state: ExecState::Completed,
            outcome: Some(Outcome::Exited(0)),
        },
    );
    assert_eq!(
        plane
            .organization()
            .resource(&rkey(1))
            .unwrap()
            .activation
            .unwrap()
            .state,
        ExecState::Completed
    );
    assert_eq!(plane.organization().journal_seq(), 7);
}

#[test]
fn a_journal_gap_refetches_the_organization() {
    let mut plane = seeded();
    let _ = commands(&mut plane);
    event(
        &mut plane,
        None,
        AgentEvent::JournalGap {
            first_missing: 6,
            last_missing: 9,
        },
    );
    assert_eq!(plane.organization().state(), ReplicaState::Seeding);
    assert!(commands(&mut plane).iter().any(|(_, c)| matches!(
        c,
        Command::GetOrganization {
            filter: OrgFilter::All,
            cursor: None,
            ..
        }
    )));
}

#[test]
fn an_operation_is_resent_with_the_same_id_after_a_reconnect_and_deduped() {
    let mut plane = seeded();
    let _ = commands(&mut plane);
    let operation = plane.rename_session(skey(1), "notes").expect("submitted");
    let sent = commands(&mut plane);
    assert!(matches!(
        sent.as_slice(),
        [(_, Command::SessionRename { operation: op, .. })] if *op == operation
    ));

    // The reply is lost with the connection.
    plane.connection_lost(Some("reset".to_owned()));
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(hello(true)).expect("HELLO_OK");
    let after_hello = commands(&mut plane);
    assert!(
        !after_hello
            .iter()
            .any(|(_, c)| matches!(c, Command::SessionRename { .. })),
        "no resend before the seed names the authority"
    );
    let seed = after_hello
        .iter()
        .find(|(_, c)| matches!(c, Command::GetOrganization { .. }))
        .unwrap()
        .0;
    answer(&mut plane, seed, page_value(page(8)));
    let resent = commands(&mut plane);
    let [(request_id, Command::SessionRename { operation: op, .. })] = resent.as_slice() else {
        panic!("expected one resend, got {resent:?}");
    };
    assert_eq!(*op, operation, "the resend reuses the operation id");
    // The server answers from its receipt.
    answer(&mut plane, *request_id, committed(operation, 3));
    let outcomes = org_outcomes(&plane.take_events());
    assert_eq!(outcomes.len(), 1);
    assert!(matches!(outcomes[0].1, OrgOutcome::Committed(_)));
    assert!(plane.pending_organization_operations().next().is_none());
    // A same-id resubmit with the same payload is journaled once.
    let command = Command::SessionRename {
        operation,
        session: skey(1),
        name: "again".to_owned(),
    };
    let first = plane.submit_organization(command.clone()).unwrap();
    plane.submit_organization(command).unwrap();
    assert_eq!(first, operation);
    assert_eq!(plane.pending_organization_operations().count(), 1);
}

#[test]
fn another_authority_after_a_reconnect_reports_the_outcome_unknown() {
    let mut plane = seeded();
    let _ = commands(&mut plane);
    let operation = plane.rename_session(skey(1), "notes").unwrap();
    let _ = commands(&mut plane);
    plane.connection_lost(None);
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(hello(true)).unwrap();
    let seed = commands(&mut plane)
        .into_iter()
        .find(|(_, c)| matches!(c, Command::GetOrganization { .. }))
        .unwrap()
        .0;
    let other = OrganizationPage::new(AuthorityId::new([2; 16]).unwrap(), Revision::new(1), 1);
    answer(&mut plane, seed, page_value(other));
    let outcomes = org_outcomes(&plane.take_events());
    assert!(matches!(
        outcomes.as_slice(),
        [(op, OrgOutcome::Unknown { .. })] if *op == operation
    ));
    assert!(
        !commands(&mut plane)
            .iter()
            .any(|(_, c)| matches!(c, Command::SessionRename { .. })),
        "never resent to another authority"
    );
}

#[test]
fn a_conflict_is_surfaced_to_the_consumer() {
    let mut plane = seeded();
    let _ = commands(&mut plane);
    let operation = plane.rename_session(skey(1), "notes").unwrap();
    let (request_id, _) = commands(&mut plane).remove(0);
    answer(
        &mut plane,
        request_id,
        CommandResult::Refused(Refusal::Conflict {
            current: Revision::new(4),
        }),
    );
    let outcomes = org_outcomes(&plane.take_events());
    assert_eq!(
        outcomes,
        vec![(
            operation,
            OrgOutcome::Refused(Refusal::Conflict {
                current: Revision::new(4)
            })
        )]
    );
}

#[test]
fn without_organization_nothing_is_read_and_submissions_are_not_sent() {
    let (mut plane, seed) = negotiated(false);
    assert!(seed.is_none(), "no GET_ORGANIZATION to an older server");
    assert_eq!(plane.organization().state(), ReplicaState::Empty);
    assert!(plane.refresh_organization().is_none());
    let operation = plane
        .submit_organization(Command::SessionDelete {
            operation: OperationId::new([5; 16]).unwrap(),
            session: skey(1),
        })
        .unwrap();
    assert!(commands(&mut plane).is_empty());
    let outcomes = org_outcomes(&plane.take_events());
    assert!(matches!(
        outcomes.as_slice(),
        [(op, OrgOutcome::NotSent { .. })] if *op == operation
    ));
    // close_terminals keeps the legacy atomic batch.
    plane.close_terminals(vec![ResourceId::local(1)]).unwrap();
    assert!(
        commands(&mut plane)
            .iter()
            .any(|(_, c)| matches!(c, Command::CloseTabResources { .. }))
    );
    assert!(plane.kill_session(skey(1)).is_none());
}

#[test]
fn close_terminals_terminates_each_under_organization() {
    let mut plane = seeded();
    let _ = commands(&mut plane);
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
fn kill_session_runs_terminate_remove_delete_and_reports_each_target() {
    let mut plane = seeded();
    let _ = commands(&mut plane);
    let workflow = plane.kill_session(skey(1)).expect("live replica");
    let sent = commands(&mut plane);
    let [(id, Command::ResourceTerminate { operation, .. })] = sent.as_slice() else {
        panic!("expected a terminate, got {sent:?}");
    };
    answer(
        &mut plane,
        *id,
        CommandResult::Accepted {
            operation: *operation,
        },
    );
    assert!(commands(&mut plane).is_empty(), "waiting for the exit");
    event(
        &mut plane,
        Some(6),
        AgentEvent::ActivationChanged {
            resource: rkey(1),
            activation: ActivationId::new(1),
            state: ExecState::Completed,
            outcome: Some(Outcome::Signaled(15)),
        },
    );
    let sent = commands(&mut plane);
    let (id, operation) = sent
        .iter()
        .find_map(|(id, c)| match c {
            Command::ResourceRemove { operation, .. } => Some((*id, *operation)),
            _ => None,
        })
        .expect("a remove");
    answer(&mut plane, id, committed(operation, 4));
    let sent = commands(&mut plane);
    let (id, operation) = sent
        .iter()
        .find_map(|(id, c)| match c {
            Command::SessionDelete { operation, .. } => Some((*id, *operation)),
            _ => None,
        })
        .expect("a delete");
    answer(&mut plane, id, committed(operation, 5));
    let events = plane.take_events();
    let report = events
        .iter()
        .find_map(|e| match e {
            Event::KillFinished {
                workflow: w,
                report,
            } if *w == workflow => Some(report.clone()),
            _ => None,
        })
        .expect("the workflow finished");
    assert_eq!(report.targets, vec![(rkey(1), TargetOutcome::Removed)]);
    assert_eq!(report.session_outcome, SessionOutcome::Deleted);
    assert!(
        org_outcomes(&events).is_empty(),
        "workflow steps are not reported as standalone operations"
    );
}

#[test]
fn the_topology_carries_keys_lifetimes_and_exec_state() {
    let mut plane = seeded();
    let _ = commands(&mut plane);
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "work").with_key(Some(skey(1))),
        ])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(1), WindowId::new(1), 80, 24)
                .with_session(Some(SessionId::new(1))),
        ]);
    let refresh = plane.refresh_topology().unwrap();
    let _ = commands(&mut plane);
    answer(
        &mut plane,
        refresh,
        CommandResult::OkWith(CommandValue::State(snapshot)),
    );
    let topology = plane.topology().expect("topology");
    assert_eq!(topology.sessions[0].key, Some(skey(1)));
    assert_eq!(topology.sessions[0].revision, Some(Revision::new(2)));
    assert_eq!(topology.sessions[0].members, 1);
    assert_eq!(topology.panes[0].session_id, 1);
    assert_eq!(topology.panes[0].key, Some(rkey(1)));
    assert_eq!(topology.panes[0].exec, Some(ExecState::Running));
    assert_eq!(topology.handles.key(&ResourceId::local(1)), Some(rkey(1)));
}

#[test]
fn membership_events_update_live_pane_ownership_without_another_state_snapshot() {
    let (mut plane, seed) = negotiated(true);
    let seed_page = page(5).with_sessions(vec![
        SessionRecord::new(skey(1), "source", Revision::new(2))
            .with_members(1)
            .with_live(Some(SessionId::new(1))),
        SessionRecord::new(skey(2), "destination", Revision::new(2))
            .with_live(Some(SessionId::new(2))),
    ]);
    answer(&mut plane, seed.unwrap(), page_value(seed_page));
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "source").with_key(Some(skey(1))),
            SessionInfo::new(SessionId::new(2), "destination").with_key(Some(skey(2))),
        ])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(1), WindowId::new(1), 80, 24)
                .with_session(Some(SessionId::new(1))),
        ]);
    let refresh = plane.refresh_topology().unwrap();
    let _ = commands(&mut plane);
    answer(
        &mut plane,
        refresh,
        CommandResult::OkWith(CommandValue::State(snapshot)),
    );
    assert_eq!(plane.topology().unwrap().panes[0].session_id, 1);
    event(
        &mut plane,
        Some(6),
        AgentEvent::MembershipChanged {
            resource: rkey(1),
            from: skey(1),
            to: skey(2),
            from_revision: Revision::new(3),
            to_revision: Revision::new(3),
        },
    );
    let topology = plane.topology().unwrap();
    assert_eq!(topology.panes[0].session_id, 2);
    assert_eq!(topology.panes[0].session_name, "destination");
    assert_eq!(topology.panes[0].terminal_id, ResourceId::local(1));
    assert_eq!(topology.panes[0].activation, Some(ActivationId::new(1)));
    event(
        &mut plane,
        Some(7),
        AgentEvent::MembershipChanged {
            resource: rkey(1),
            from: skey(2),
            to: skey(1),
            from_revision: Revision::new(4),
            to_revision: Revision::new(4),
        },
    );
    assert_eq!(plane.topology().unwrap().panes[0].session_id, 1);
    assert_eq!(plane.topology().unwrap().panes[0].session_name, "source");
}
