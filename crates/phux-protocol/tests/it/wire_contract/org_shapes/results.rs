//! Strategies for the organization results.

use super::{
    actor, arrangement_key, document, exec_state, kind, lifetime, live_target, name, nonzero16, op,
    outcome, owner, pairs, pin, resource_id, resource_key, rev, session_key,
};
use phux_protocol::ids::{ActivationId, AuthorityId, ServerInstance, SessionId};
use phux_protocol::kinds::Verb;
use phux_protocol::wire::frame::{
    ActivationInfo, ArrangementValue, CommandResult, CommandValue, Committed, Delegation,
    ExitPolicy, OrganizationPage, Refusal, ResourceRecord, SessionRecord,
};
use proptest::prelude::*;

fn refusal() -> impl Strategy<Value = Refusal> {
    prop_oneof![
        proptest::sample::select(Verb::ALL.to_vec())
            .prop_map(|verb| Refusal::Unauthorized { verb }),
        Just(Refusal::NotFound),
        rev().prop_map(|current| Refusal::Conflict { current }),
        proptest::option::of(any::<u64>()).prop_map(|current| Refusal::Stale {
            current: current.map(ActivationId::new),
        }),
        any::<u32>().prop_map(|members| Refusal::NotEmpty { members }),
        name().prop_map(|capability| Refusal::Unsupported { capability }),
        (name(), name()).prop_map(|(field, reason)| Refusal::InvalidArgument { field, reason }),
        Just(Refusal::NameTaken),
        Just(Refusal::ExecutionUncertain),
        actor().prop_map(|holder| Refusal::LeaseHeld { holder }),
        Just(Refusal::DuplicateOperation),
        name().prop_map(|reason| Refusal::StoreFailure { reason }),
    ]
}

#[allow(
    clippy::too_many_lines,
    reason = "one strategy per organization result"
)]
pub(super) fn org_result() -> impl Strategy<Value = CommandResult> {
    let session_record = (
        session_key(),
        name(),
        lifetime(),
        any::<bool>(),
        proptest::option::of(arrangement_key()),
        pairs(),
        any::<u32>(),
        rev(),
        proptest::option::of(any::<u32>()),
    )
        .prop_map(
            |(key, n, l, awaiting, arrangement, attrs, members, r, live)| {
                SessionRecord::new(key, n, r)
                    .with_lifetime(l)
                    .with_awaiting_organization(awaiting)
                    .with_arrangement(arrangement)
                    .with_attrs(attrs)
                    .with_members(members)
                    .with_live(live.map(SessionId::new))
            },
        );
    let activation = (
        any::<u64>(),
        any::<[u8; 16]>(),
        exec_state(),
        proptest::option::of(outcome()),
    )
        .prop_map(|(id, instance, state, outcome)| {
            ActivationInfo::new(ActivationId::new(id), ServerInstance::new(instance), state)
                .with_outcome(outcome)
        });
    let resource_record = (
        resource_key(),
        session_key(),
        proptest::option::of(name()),
        kind(),
        proptest::option::of(activation),
        rev(),
        proptest::option::of(resource_id()),
    )
        .prop_map(|(key, session, n, k, activation, r, live)| {
            ResourceRecord::new(key, session, r)
                .with_name(n)
                .with_kind(k)
                .with_on_exit(ExitPolicy::Retain)
                .with_activation(activation)
                .with_live(live)
        });
    let page = (
        nonzero16(),
        proptest::option::of(session_key()),
        proptest::collection::vec(session_record, 0..3),
        proptest::collection::vec(resource_record, 0..3),
        any::<(u32, u32, u32)>(),
        rev(),
        any::<u64>(),
        proptest::option::of(proptest::collection::vec(any::<u8>(), 0..16)),
    )
        .prop_map(
            |(authority, inbox, sessions, resources, totals, r, seq, cursor)| {
                CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(
                    OrganizationPage::new(AuthorityId::new(authority).unwrap(), r, seq)
                        .with_inbox(inbox)
                        .with_sessions(sessions)
                        .with_resources(resources)
                        .with_totals(totals.0, totals.1, totals.2)
                        .with_next_cursor(cursor),
                )))
            },
        );
    let committed = (
        op(),
        rev(),
        proptest::option::of(session_key()),
        proptest::option::of(resource_key()),
        proptest::option::of(live_target()),
        proptest::collection::vec(pin(), 0..3),
        proptest::option::of(any::<bool>()),
        proptest::option::of(any::<u32>()),
    )
        .prop_map(
            |(operation, r, session, resource, target, pins, prior, members)| {
                CommandResult::OkWith(CommandValue::Committed(Box::new(
                    Committed::new(operation, r)
                        .with_session(session)
                        .with_resource(resource)
                        .with_target(target)
                        .with_revisions(pins)
                        .with_prior_awaiting(prior)
                        .with_members(members),
                )))
            },
        );
    let arrangement = (arrangement_key(), owner(), rev(), document()).prop_map(|(k, o, r, d)| {
        CommandResult::OkWith(CommandValue::Arrangement(Box::new(ArrangementValue::new(
            k, o, r, d,
        ))))
    });
    let delegation = (
        session_key(),
        "[a-z0-9:]{1,64}",
        "/[a-z0-9/._-]{0,80}",
        any::<u64>(),
    )
        .prop_map(|(session, id, path, expires)| {
            CommandResult::OkWith(CommandValue::Delegation(Box::new(Delegation::new(
                id, session, path, expires,
            ))))
        });
    prop_oneof![
        refusal().prop_map(CommandResult::Refused),
        op().prop_map(|operation| CommandResult::Accepted { operation }),
        page,
        committed,
        arrangement,
        delegation,
    ]
}
