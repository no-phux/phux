//! Bulk moves use one receipt per chunk, with fresh inventory between chunks.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use phux_client_core::org_ops::{ORG_JOURNAL_MAX_OPS, OrgOutcome, SubmitError};
use phux_client_runtime::control::{ControlOptions, ControlPlane, Event};
use phux_protocol::caps::{ServerCapabilities, ServerFeatureExt, ServerFeatureExtSet};
use phux_protocol::ids::{
    ActivationId, AuthorityId, OperationId, ResourceId, ResourceKey, Revision, ServerInstance,
    SessionKey,
};
use phux_protocol::kinds::Verb;
use phux_protocol::wire::frame::{
    ActivationInfo, AgentEvent, Command, CommandResult, CommandValue, Committed, ErrorCode,
    EventStamp, ExecState, FrameKind, MAX_ORG_BATCH, OrganizationPage, Refusal, ResourceRecord,
    SessionLifetime, SessionRecord,
};

use crate::embedded::{answer, commands};

mod fixtures;
use fixtures::*;

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
        assert_eq!(ids(&outcomes), operations);
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
    assert_eq!(ids(&results), operations);
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
        // The post-receipt inventory reply is lost.
        let (mut plane, operations, _) = first_chunk_committed();
        reconnect(&mut plane, true);
        seed(&mut plane, 257, 256, 3, authority);
        if authority == 1 {
            let (request, _) = chunk(&mut plane, operations[1], 256, 257, 3);
            commit(&mut plane, request, operations[1], 4);
        } else {
            assert!(commands(&mut plane).is_empty());
            assert_tail_cancelled(&mut plane);
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
        assert_eq!(ids(&results), operations);
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
        let (mut plane, operations, _) = first_chunk_sent();
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
        assert_eq!(ids(&results), operations);
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
fn an_inventory_started_before_the_receipt_is_not_the_chunk_barrier() {
    let (mut plane, operations, request) = first_chunk_sent();
    plane.refresh_organization().unwrap();
    commit(&mut plane, request, operations[0], 3);
    seed(&mut plane, 257, 0, 2, 1); // This cut predates the commit.
    seed(&mut plane, 257, 256, 3, 1); // A fresh read, not a stale next move.
    chunk(&mut plane, operations[1], 256, 257, 3);
}

#[test]
fn refresh_accounts_for_changed_sources_and_members_already_at_destination() {
    let (mut plane, operations, request) = first_chunk_sent();
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
    let (mut plane, operations, read) = first_chunk_committed();
    let mut first = inventory(257, 256, 3, 1);
    let second_resources = first.resources.split_off(128);
    answer_page(&mut plane, read, first.with_next_cursor(Some(vec![1])));
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
    answer_page(
        &mut plane,
        *request,
        OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(3), 5)
            .with_resources(second_resources),
    );
    chunk(&mut plane, operations[1], 256, 257, 3);
}

#[test]
fn a_resource_removed_between_chunks_cancels_remaining_operations() {
    let (mut plane, operations, request) = first_chunk_sent();
    commit(&mut plane, request, operations[0], 3);
    seed(&mut plane, 256, 256, 3, 1);
    assert!(commands(&mut plane).is_empty());
    assert_tail_cancelled(&mut plane);
}

#[test]
fn acceptance_without_a_commit_receipt_does_not_advance_bulk_work() {
    let (mut plane, operations, request) = first_chunk_sent();
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

/// A failed or terminally refused post-receipt inventory read cancels only
/// the unsubmitted tail, and the first page is not read again.
#[test]
fn terminal_inventory_refusals_cancel_tails_without_retrying_the_first_page() {
    let refused = |refusal| CommandResult::Refused(refusal);
    for (label, answer_read) in [
        (
            "a_failed_inventory_read_cancels_only_the_unsubmitted_tail",
            read_failed(),
        ),
        (
            "unauthorized",
            refused(Refusal::Unauthorized {
                verb: Verb::Inventory,
            }),
        ),
        (
            "unsupported",
            refused(Refusal::Unsupported {
                capability: "organization".to_owned(),
            }),
        ),
        (
            "store failure",
            refused(Refusal::StoreFailure {
                reason: "disk unavailable".to_owned(),
            }),
        ),
        ("expired cursor", expired_cursor()),
    ] {
        let (mut plane, _, read) = first_chunk_committed();
        answer(&mut plane, read, answer_read);
        assert!(commands(&mut plane).is_empty(), "{label}");
        assert_tail_cancelled_in(&mut plane, label);
        assert_eq!(
            plane.pending_organization_operations().count(),
            0,
            "{label}"
        );
    }
}

#[test]
fn a_rejected_continuation_restarts_once_but_never_loops() {
    let (mut plane, operations, request) = first_chunk_sent();
    commit(&mut plane, request, operations[0], 3);
    for attempt in 0..2 {
        let (request, _) = commands(&mut plane).remove(0);
        answer_page(
            &mut plane,
            request,
            inventory(128, 128, 3, 1).with_next_cursor(Some(vec![1])),
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
            panic!("one continuation expected: {sent:?}");
        };
        answer(&mut plane, *request, expired_cursor());
        if attempt == 0 {
            assert_eq!(plane.pending_organization_operations().count(), 1);
        }
    }
    assert!(commands(&mut plane).is_empty());
    assert_tail_cancelled(&mut plane);
    assert_eq!(plane.pending_organization_operations().count(), 0);
}

#[test]
fn one_cursor_restart_can_recover_and_send_the_reserved_chunk() {
    let (mut plane, operations, read) = first_chunk_committed();
    answer_page(
        &mut plane,
        read,
        inventory(128, 128, 3, 1).with_next_cursor(Some(vec![1])),
    );
    let (request, _) = commands(&mut plane).remove(0);
    answer(&mut plane, request, expired_cursor());
    seed(&mut plane, 257, 256, 3, 1);
    let (request, _) = chunk(&mut plane, operations[1], 256, 257, 3);
    commit(&mut plane, request, operations[1], 4);
    assert!(
        outcomes(&mut plane)
            .iter()
            .all(|(_, outcome)| matches!(outcome, OrgOutcome::Committed(_)))
    );
    assert_eq!(plane.pending_organization_operations().count(), 0);
}

#[test]
fn a_failed_refresh_before_the_receipt_resolves_the_tail_without_manual_recovery() {
    let (mut plane, operations, request) = first_chunk_sent();
    let refresh = plane.refresh_organization().unwrap();
    commands(&mut plane);
    answer(&mut plane, refresh, read_failed());
    assert!(
        matches!(outcomes(&mut plane).as_slice(), [(id, OrgOutcome::NotSent { .. })] if *id == operations[1])
    );
    assert_eq!(plane.pending_organization_operations().count(), 1);
    commit(&mut plane, request, operations[0], 3);
    assert!(
        matches!(outcomes(&mut plane).as_slice(), [(id, OrgOutcome::Committed(_))] if *id == operations[0])
    );
    assert_eq!(plane.pending_organization_operations().count(), 0);
    assert!(commands(&mut plane).is_empty());
}

/// Fill the journal with `n` renames, discarding the commands they send.
fn rename_n(plane: &mut ControlPlane, n: usize) {
    for _ in 0..n {
        plane.rename_session(session(1), "new name").unwrap();
    }
    commands(plane);
}

#[test]
fn ordinary_and_internal_producers_cannot_steal_an_accepted_bulk_reservation() {
    let mut plane = plane(257);
    let operations = plane.move_resources(&selection(257), session(3)).unwrap();
    let (request, retry) = chunk(&mut plane, operations[0], 0, 256, 2);
    rename_n(&mut plane, ORG_JOURNAL_MAX_OPS - 2);
    assert_eq!(
        plane.pending_organization_operations().count(),
        ORG_JOURNAL_MAX_OPS
    );
    assert_eq!(plane.submit_organization(retry).unwrap(), operations[0]);
    assert_eq!(
        plane.rename_session(session(1), "overflow"),
        Err(SubmitError::Full)
    );
    let close = plane.close_terminals(vec![ResourceId::local(1)]).unwrap();
    let events = plane.take_events();
    assert!(events.iter().any(|event| matches!(event, Event::TerminalsClosed { request_id, error: Some(_) } if *request_id == close)));
    assert_eq!(plane.kill_session(session(1)), Err(SubmitError::Full));
    assert!(commands(&mut plane).is_empty());
    assert_eq!(
        plane.pending_organization_operations().count(),
        ORG_JOURNAL_MAX_OPS
    );
    commit(&mut plane, request, operations[0], 3);
    seed(&mut plane, 257, 256, 3, 1);
    let (request, _) = chunk(&mut plane, operations[1], 256, 257, 3);
    assert_eq!(
        plane.pending_organization_operations().count(),
        ORG_JOURNAL_MAX_OPS - 1
    );
    commit(&mut plane, request, operations[1], 4);
}

#[test]
fn live_kill_and_close_can_fill_only_unreserved_slots() {
    for use_kill in [false, true] {
        let (mut plane, operations, request) = first_chunk_sent();
        rename_n(&mut plane, ORG_JOURNAL_MAX_OPS - 3);
        if use_kill {
            plane.kill_session(session(1)).unwrap();
        } else {
            plane
                .close_terminals(vec![ResourceId::local(1), ResourceId::local(2)])
                .unwrap();
        }
        let sent = commands(&mut plane);
        assert!(matches!(
            sent.as_slice(),
            [(
                _,
                Command::ResourceTerminate { .. } | Command::SessionKill { .. }
            )]
        ));
        assert_eq!(
            plane.pending_organization_operations().count(),
            ORG_JOURNAL_MAX_OPS
        );
        assert_eq!(
            plane.rename_session(session(1), "overflow"),
            Err(SubmitError::Full)
        );
        commit(&mut plane, request, operations[0], 3);
        seed(&mut plane, 257, 256, 3, 1);
        chunk(&mut plane, operations[1], 256, 257, 3);
        assert_eq!(
            plane.pending_organization_operations().count(),
            ORG_JOURNAL_MAX_OPS - 1
        );
    }
}
