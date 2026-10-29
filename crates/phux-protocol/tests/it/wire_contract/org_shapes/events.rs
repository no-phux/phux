//! Strategies for the organization events.

use super::{
    actor, arrangement_key, exec_state, kind, lifetime, name, outcome, owner, resource_key, rev,
    session_key,
};
use phux_protocol::ids::{ActivationId, LeaseEpoch};
use phux_protocol::wire::frame::AgentEvent;
use proptest::prelude::*;

#[allow(
    clippy::too_many_lines,
    reason = "one arm per organization event, kept as one flat table"
)]
pub(super) fn org_event() -> impl Strategy<Value = AgentEvent> {
    prop_oneof![
        (session_key(), name(), lifetime(), rev()).prop_map(
            |(session, name, lifetime, revision)| {
                AgentEvent::SessionCreated {
                    session,
                    name,
                    lifetime,
                    revision,
                }
            }
        ),
        (session_key(), name(), rev()).prop_map(|(session, name, revision)| {
            AgentEvent::SessionRenamed {
                session,
                name,
                revision,
            }
        }),
        (session_key(), proptest::option::of(name()), rev()).prop_map(
            |(session, project, revision)| AgentEvent::SessionProjectSet {
                session,
                project,
                revision,
            }
        ),
        (session_key(), name(), rev()).prop_map(|(session, name, revision)| {
            AgentEvent::SessionDeleted {
                session,
                name,
                revision,
            }
        }),
        (session_key(), lifetime(), rev()).prop_map(|(session, lifetime, revision)| {
            AgentEvent::SessionLifetimeChanged {
                session,
                lifetime,
                revision,
            }
        }),
        (resource_key(), session_key(), session_key(), rev(), rev()).prop_map(
            |(resource, from, to, from_revision, to_revision)| AgentEvent::MembershipChanged {
                resource,
                from,
                to,
                from_revision,
                to_revision,
            }
        ),
        (resource_key(), session_key(), kind()).prop_map(|(resource, session, kind)| {
            AgentEvent::ResourceCreated {
                resource,
                session,
                kind,
            }
        }),
        (resource_key(), session_key(), kind()).prop_map(|(resource, session, kind)| {
            AgentEvent::ResourceRemoved {
                resource,
                session,
                kind,
            }
        }),
        (resource_key(), proptest::option::of(name()), rev()).prop_map(
            |(resource, name, revision)| AgentEvent::ResourceRenamed {
                resource,
                name,
                revision,
            }
        ),
        (
            resource_key(),
            any::<u64>(),
            exec_state(),
            proptest::option::of(outcome())
        )
            .prop_map(
                |(resource, a, state, outcome)| AgentEvent::ActivationChanged {
                    resource,
                    activation: ActivationId::new(a),
                    state,
                    outcome,
                }
            ),
        (arrangement_key(), owner(), proptest::option::of(rev())).prop_map(
            |(arrangement, owner, revision)| AgentEvent::ArrangementChanged {
                arrangement,
                owner,
                revision,
            }
        ),
        (resource_key(), any::<u64>(), proptest::option::of(actor())).prop_map(
            |(resource, epoch, holder)| AgentEvent::LeaseChanged {
                resource,
                epoch: LeaseEpoch::new(epoch),
                holder,
            }
        ),
        (session_key(), any::<bool>(), rev()).prop_map(|(session, awaiting, revision)| {
            AgentEvent::IntakeChanged {
                session,
                awaiting_organization: awaiting,
                revision,
            }
        }),
    ]
}
