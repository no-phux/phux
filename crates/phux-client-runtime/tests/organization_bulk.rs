//! Bulk moves use one receipt per chunk, with fresh inventory between chunks.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use phux_client_core::org_ops::{ORG_JOURNAL_MAX_OPS, OrgOutcome, SubmitError};
use phux_client_runtime::control::{ControlOptions, ControlPlane, Event};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ServerCapabilities, ServerFeatureExt, ServerFeatureExtSet,
};
use phux_protocol::ids::{AuthorityId, OperationId, ResourceKey, Revision, SessionKey};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, Committed, ErrorCode, EventStamp, FrameKind,
    MAX_ORG_BATCH, OrganizationPage, Refusal, ResourceRecord, SessionLifetime, SessionRecord,
};

const fn session(n: u8) -> SessionKey {
    SessionKey::new([n; 16]).unwrap()
}

fn resource(n: usize) -> ResourceKey {
    let mut key = [0x40; 16];
    key[..8].copy_from_slice(&(n as u64).to_le_bytes());
    ResourceKey::new(key).unwrap()
}

fn hello(supported: bool) -> FrameKind {
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new().with_features_ext(if supported {
            ServerFeatureExtSet::with(&[ServerFeatureExt::Organization])
        } else {
            ServerFeatureExtSet::new()
        }),
        server_id: vec![0xAB; 16],
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    }
}

fn commands(plane: &mut ControlPlane) -> Vec<(u32, Command)> {
    plane
        .take_outbound()
        .iter()
        .filter_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
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
        .unwrap();
}

fn inventory(count: usize, moved: usize, revision: u64, authority: u8) -> OrganizationPage {
    OrganizationPage::new(
        AuthorityId::new([authority; 16]).unwrap(),
        Revision::new(revision),
        5,
    )
    .with_sessions(
        (1..=3)
            .map(|n| {
                SessionRecord::new(session(n), format!("session-{n}"), Revision::new(revision))
                    .with_lifetime(SessionLifetime::Persistent)
                    .with_members(
                        u32::try_from(
                            (0..count)
                                .filter(|index| {
                                    let owner = if *index < moved {
                                        session(3)
                                    } else {
                                        source(*index)
                                    };
                                    owner == session(n)
                                })
                                .count(),
                        )
                        .unwrap(),
                    )
            })
            .collect(),
    )
    .with_resources(
        (0..count)
            .map(|n| {
                ResourceRecord::new(
                    resource(n),
                    if n < moved { session(3) } else { source(n) },
                    Revision::new(1),
                )
            })
            .collect(),
    )
}

const fn source(n: usize) -> SessionKey {
    if n.is_multiple_of(2) {
        session(1)
    } else {
        session(2)
    }
}

fn seed(plane: &mut ControlPlane, count: usize, moved: usize, revision: u64, authority: u8) {
    let mut all = inventory(count, moved, revision, authority);
    let resources = std::mem::take(&mut all.resources);
    let page_count = resources.len().div_ceil(128);
    for (index, chunk) in resources.chunks(128).enumerate() {
        let request = commands(plane)
            .into_iter()
            .find_map(|(id, command)| {
                matches!(command, Command::GetOrganization { .. }).then_some(id)
            })
            .expect("inventory read");
        let mut page = all.clone().with_resources(chunk.to_vec());
        if index > 0 {
            page.sessions.clear();
        }
        if index + 1 < page_count {
            page.next_cursor = Some(vec![u8::try_from(index + 1).unwrap()]);
        }
        answer(
            plane,
            request,
            CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
        );
    }
}

fn plane(count: usize) -> ControlPlane {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    plane.take_outbound();
    plane.feed(hello(true)).unwrap();
    seed(&mut plane, count, 0, 2, 1);
    commands(&mut plane);
    plane.take_events();
    plane
}

fn selection(count: usize) -> Vec<ResourceKey> {
    (0..count).map(resource).collect()
}

fn chunk(
    plane: &mut ControlPlane,
    expected: OperationId,
    start: usize,
    end: usize,
    revision: u64,
) -> (u32, Command) {
    let sent = commands(plane);
    let [
        (
            request,
            command @ Command::ResourceMoveMany {
                operation,
                items,
                to,
                to_expected,
            },
        ),
    ] = sent.as_slice()
    else {
        panic!("one move chunk expected: {sent:?}");
    };
    assert_eq!(*operation, expected);
    assert_eq!(*to, session(3));
    assert_eq!(*to_expected, Revision::new(revision));
    assert_eq!(items.len(), end - start);
    for (n, item) in (start..end).zip(items) {
        assert_eq!(item.resource, resource(n));
        assert_eq!(item.from, source(n));
        assert_eq!(item.from_expected, Revision::new(revision));
    }
    (*request, command.clone())
}

fn commit(plane: &mut ControlPlane, request: u32, operation: OperationId, revision: u64) {
    answer(
        plane,
        request,
        CommandResult::OkWith(CommandValue::Committed(Box::new(Committed::new(
            operation,
            Revision::new(revision),
        )))),
    );
}

fn outcomes(plane: &mut ControlPlane) -> Vec<(OperationId, OrgOutcome)> {
    plane
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            Event::OrganizationOperation { operation, outcome } => Some((operation, outcome)),
            _ => None,
        })
        .collect()
}

fn reconnect(plane: &mut ControlPlane, supported: bool) {
    plane.connection_lost(None);
    plane.connection_opened();
    plane.take_outbound();
    plane.feed(hello(supported)).unwrap();
}

#[test]
fn three_chunks_refresh_revisions_and_deliver_each_receipt_in_order() {
    for events_first in [false, true] {
        let mut plane = plane(600);
        let operations = plane.move_resources(&selection(600), session(3)).unwrap();
        assert_eq!(operations.len(), 3);
        assert_eq!(plane.pending_organization_operations().count(), 3);
        for (index, operation) in operations.iter().enumerate() {
            let start = index * MAX_ORG_BATCH;
            let end = (start + MAX_ORG_BATCH).min(600);
            let revision = index as u64 + 2;
            let (request, _) = chunk(&mut plane, *operation, start, end, revision);
            if !events_first {
                commit(&mut plane, request, *operation, revision + 1);
            }
            // Membership deltas may precede or follow the receipt. No later
            // move is sent before the post-receipt inventory completes.
            for n in start..end {
                plane
                    .feed(FrameKind::Event {
                        terminal: None,
                        event: AgentEvent::MembershipChanged {
                            resource: resource(n),
                            from: source(n),
                            to: session(3),
                            from_revision: Revision::new(revision + 1),
                            to_revision: Revision::new(revision + 1),
                        },
                        stamp: Some(Box::new(EventStamp::new(n as u64 + 6, 0))),
                    })
                    .unwrap();
            }
            if events_first {
                assert!(commands(&mut plane).is_empty());
                commit(&mut plane, request, *operation, revision + 1);
            }
            if index < 2 {
                seed(&mut plane, 600, end, revision + 1, 1);
            }
        }
        let outcomes = outcomes(&mut plane);
        assert_eq!(
            outcomes.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            operations
        );
        assert!(
            outcomes
                .iter()
                .all(|(_, result)| matches!(result, OrgOutcome::Committed(_)))
        );
        assert_eq!(plane.pending_organization_operations().count(), 0);
    }
}

#[test]
fn refusal_of_second_chunk_preserves_first_receipt_and_cancels_tail() {
    let mut plane = plane(600);
    let operations = plane.move_resources(&selection(600), session(3)).unwrap();
    let (request, _) = chunk(&mut plane, operations[0], 0, 256, 2);
    commit(&mut plane, request, operations[0], 3);
    seed(&mut plane, 600, 256, 3, 1);
    let (request, _) = chunk(&mut plane, operations[1], 256, 512, 3);
    answer(
        &mut plane,
        request,
        CommandResult::Refused(Refusal::Conflict {
            current: Revision::new(9),
        }),
    );
    let results = outcomes(&mut plane);
    assert!(matches!(
        results.as_slice(),
        [
            (_, OrgOutcome::Committed(_)),
            (_, OrgOutcome::Refused(_)),
            (_, OrgOutcome::NotSent { .. })
        ]
    ));
    assert_eq!(
        results.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        operations
    );
    assert!(commands(&mut plane).is_empty());
    assert_eq!(plane.pending_organization_operations().count(), 0);
}

#[test]
fn reconnect_retries_only_current_payload_verbatim_then_uses_fresh_inventory() {
    let mut plane = plane(257);
    let operations = plane.move_resources(&selection(257), session(3)).unwrap();
    let (_, original) = chunk(&mut plane, operations[0], 0, 256, 2);
    reconnect(&mut plane, true);
    seed(&mut plane, 257, 256, 3, 1);
    let [(request, retry)] = commands(&mut plane).try_into().unwrap();
    assert_eq!(retry, original);
    commit(&mut plane, request, operations[0], 3);
    seed(&mut plane, 257, 256, 3, 1);
    let (request, _) = chunk(&mut plane, operations[1], 256, 257, 3);
    commit(&mut plane, request, operations[1], 4);
    assert_eq!(outcomes(&mut plane).len(), 2);
}

#[test]
fn reconnect_during_post_receipt_seed_resumes_or_cancels_on_authority_change() {
    for authority in [1, 2] {
        let mut plane = plane(257);
        let operations = plane.move_resources(&selection(257), session(3)).unwrap();
        let (request, _) = chunk(&mut plane, operations[0], 0, 256, 2);
        commit(&mut plane, request, operations[0], 3);
        commands(&mut plane); // Lose the post-receipt inventory reply.
        reconnect(&mut plane, true);
        seed(&mut plane, 257, 256, 3, authority);
        if authority == 1 {
            let (request, _) = chunk(&mut plane, operations[1], 256, 257, 3);
            commit(&mut plane, request, operations[1], 4);
        } else {
            assert!(commands(&mut plane).is_empty());
            assert!(matches!(
                outcomes(&mut plane).as_slice(),
                [
                    (_, OrgOutcome::Committed(_)),
                    (_, OrgOutcome::NotSent { .. })
                ]
            ));
        }
    }
}

#[test]
fn unknown_or_not_sent_current_chunk_never_sends_remaining_work() {
    for attempted in [false, true] {
        let mut plane = plane(600);
        if !attempted {
            plane.connection_lost(None);
        }
        let operations = plane.move_resources(&selection(600), session(3)).unwrap();
        commands(&mut plane);
        plane.close();
        let results = outcomes(&mut plane);
        assert_eq!(
            results.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            operations
        );
        assert_eq!(results.len(), 3);
        assert_eq!(
            matches!(results[0].1, OrgOutcome::Unknown { .. }),
            attempted
        );
        assert!(
            results[1..]
                .iter()
                .all(|(_, outcome)| matches!(outcome, OrgOutcome::NotSent { .. }))
        );
        assert_eq!(plane.pending_organization_operations().count(), 0);
    }
}

#[test]
fn reconnect_to_another_authority_or_unsupported_server_cancels_tail() {
    for supported in [false, true] {
        let mut plane = plane(257);
        let operations = plane.move_resources(&selection(257), session(3)).unwrap();
        chunk(&mut plane, operations[0], 0, 256, 2);
        reconnect(&mut plane, supported);
        if supported {
            seed(&mut plane, 257, 0, 2, 2);
        }
        assert!(
            !commands(&mut plane)
                .iter()
                .any(|(_, command)| matches!(command, Command::ResourceMoveMany { .. }))
        );
        let results = outcomes(&mut plane);
        assert!(matches!(
            results.as_slice(),
            [
                (_, OrgOutcome::Unknown { .. }),
                (_, OrgOutcome::NotSent { .. })
            ]
        ));
        assert_eq!(
            results.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            operations
        );
    }
}

#[test]
fn invalid_selection_and_full_capacity_submit_nothing() {
    let mut plane = plane(257);
    let mut invalid = selection(257);
    invalid[256] = resource(999);
    assert_eq!(
        plane.move_resources(&invalid, session(3)),
        Err(SubmitError::NotAnOperation)
    );
    invalid[256] = resource(0); // Duplicate across the chunk boundary.
    assert_eq!(
        plane.move_resources(&invalid, session(3)),
        Err(SubmitError::NotAnOperation)
    );
    assert_eq!(
        plane.move_resources(&selection(257), session(9)),
        Err(SubmitError::NotAnOperation)
    );
    assert_eq!(plane.move_resources(&[], session(3)).unwrap(), vec![]);
    assert!(commands(&mut plane).is_empty());
    assert_eq!(plane.pending_organization_operations().count(), 0);
    for _ in 0..ORG_JOURNAL_MAX_OPS - 1 {
        plane.rename_session(session(1), "renamed").unwrap();
    }
    commands(&mut plane);
    assert_eq!(
        plane.move_resources(&selection(257), session(3)),
        Err(SubmitError::Full)
    );
    assert!(commands(&mut plane).is_empty());
    assert_eq!(
        plane.pending_organization_operations().count(),
        ORG_JOURNAL_MAX_OPS - 1
    );
}

#[test]
fn a_failed_inventory_read_cancels_only_the_unsubmitted_tail() {
    let mut plane = plane(257);
    let operations = plane.move_resources(&selection(257), session(3)).unwrap();
    let (request, _) = chunk(&mut plane, operations[0], 0, 256, 2);
    commit(&mut plane, request, operations[0], 3);
    let (request, _) = commands(&mut plane).remove(0);
    answer(
        &mut plane,
        request,
        CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message: "read failed".to_owned(),
        },
    );
    assert!(matches!(
        outcomes(&mut plane).as_slice(),
        [
            (_, OrgOutcome::Committed(_)),
            (_, OrgOutcome::NotSent { .. })
        ]
    ));
    assert_eq!(plane.pending_organization_operations().count(), 0);
}

#[test]
fn an_inventory_started_before_the_receipt_is_not_the_chunk_barrier() {
    let mut plane = plane(257);
    let operations = plane.move_resources(&selection(257), session(3)).unwrap();
    let (request, _) = chunk(&mut plane, operations[0], 0, 256, 2);
    plane.refresh_organization().unwrap();
    commit(&mut plane, request, operations[0], 3);
    seed(&mut plane, 257, 0, 2, 1); // This cut predates the commit.
    seed(&mut plane, 257, 256, 3, 1); // A fresh read, not a stale next move.
    chunk(&mut plane, operations[1], 256, 257, 3);
}

#[test]
fn refresh_accounts_for_changed_sources_and_members_already_at_destination() {
    let mut plane = plane(257);
    let operations = plane.move_resources(&selection(257), session(3)).unwrap();
    let (request, _) = chunk(&mut plane, operations[0], 0, 256, 2);
    commit(&mut plane, request, operations[0], 3);
    seed(&mut plane, 257, 257, 4, 1); // Another client also moved the last member.
    let sent = commands(&mut plane);
    assert!(
        matches!(sent.as_slice(), [(_, Command::ResourceMoveMany { operation, items, to_expected, .. })]
        if *operation == operations[1] && *to_expected == Revision::new(4)
            && items[0].from == session(3) && items[0].from_expected == Revision::new(4))
    );
}

#[test]
fn no_next_chunk_before_all_post_commit_inventory_pages_arrive() {
    let mut plane = plane(257);
    let operations = plane.move_resources(&selection(257), session(3)).unwrap();
    let (request, _) = chunk(&mut plane, operations[0], 0, 256, 2);
    commit(&mut plane, request, operations[0], 3);
    let (request, _) = commands(&mut plane).remove(0);
    let mut first = inventory(257, 256, 3, 1);
    let second_resources = first.resources.split_off(128);
    answer(
        &mut plane,
        request,
        CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(
            first.with_next_cursor(Some(vec![1])),
        ))),
    );
    let sent = commands(&mut plane);
    let [
        (
            request,
            Command::GetOrganization {
                cursor: Some(_), ..
            },
        ),
    ] = sent.as_slice()
    else {
        panic!("only the next read page: {sent:?}");
    };
    answer(
        &mut plane,
        *request,
        CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(
            OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(3), 5)
                .with_resources(second_resources),
        ))),
    );
    chunk(&mut plane, operations[1], 256, 257, 3);
}

#[test]
fn a_resource_removed_between_chunks_cancels_remaining_operations() {
    let mut plane = plane(257);
    let operations = plane.move_resources(&selection(257), session(3)).unwrap();
    let (request, _) = chunk(&mut plane, operations[0], 0, 256, 2);
    commit(&mut plane, request, operations[0], 3);
    seed(&mut plane, 256, 256, 3, 1);
    assert!(commands(&mut plane).is_empty());
    assert!(matches!(
        outcomes(&mut plane).as_slice(),
        [
            (_, OrgOutcome::Committed(_)),
            (_, OrgOutcome::NotSent { .. })
        ]
    ));
}

#[test]
fn acceptance_without_a_commit_receipt_does_not_advance_bulk_work() {
    let mut plane = plane(257);
    let operations = plane.move_resources(&selection(257), session(3)).unwrap();
    let (request, _) = chunk(&mut plane, operations[0], 0, 256, 2);
    answer(
        &mut plane,
        request,
        CommandResult::Accepted {
            operation: operations[0],
        },
    );
    assert!(commands(&mut plane).is_empty());
    assert!(matches!(
        outcomes(&mut plane).as_slice(),
        [(_, OrgOutcome::Accepted), (_, OrgOutcome::NotSent { .. })]
    ));
}
