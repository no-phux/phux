//! The organization event, as its wire frame.

use phux_protocol::wire::frame;

use super::{
    activation_id, arrangement_key, exec_state, lifetime, outcome, owner, resource_key, revision,
    session_key,
};

/// A core organization event as the journal's `AgentEvent`; `None` when a
/// key cannot be carried.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one arm per event, mirrored field for field"
)]
pub fn event(e: &phux_core::Event, org: &phux_core::Org) -> Option<frame::AgentEvent> {
    use phux_core::Event as E;
    Some(match e {
        E::SessionCreated {
            key,
            name,
            revision: r,
        } => frame::AgentEvent::SessionCreated {
            session: session_key(*key)?,
            name: name.clone(),
            lifetime: org
                .session(key)
                .map_or(frame::SessionLifetime::Persistent, |s| lifetime(s.lifetime)),
            revision: revision(*r),
        },
        E::SessionRenamed {
            key,
            name,
            revision: r,
        } => frame::AgentEvent::SessionRenamed {
            session: session_key(*key)?,
            name: name.clone(),
            revision: revision(*r),
        },
        E::SessionProjectSet {
            key,
            project,
            revision: r,
        } => frame::AgentEvent::SessionProjectSet {
            session: session_key(*key)?,
            project: project.clone(),
            revision: revision(*r),
        },
        E::SessionLifetimeChanged {
            key,
            lifetime: l,
            revision: r,
        } => frame::AgentEvent::SessionLifetimeChanged {
            session: session_key(*key)?,
            lifetime: lifetime(*l),
            revision: revision(*r),
        },
        E::SessionDeleted {
            key,
            name,
            revision: r,
        } => frame::AgentEvent::SessionDeleted {
            session: session_key(*key)?,
            name: name.clone(),
            revision: revision(*r),
        },
        E::SessionIntakeChanged {
            session,
            awaiting_organization,
            revision: r,
        } => frame::AgentEvent::IntakeChanged {
            session: session_key(*session)?,
            awaiting_organization: *awaiting_organization,
            revision: revision(*r),
        },
        E::MembershipChanged {
            resource,
            from,
            to,
            from_revision,
            to_revision,
            ..
        } => frame::AgentEvent::MembershipChanged {
            resource: resource_key(*resource)?,
            from: session_key(*from)?,
            to: session_key(*to)?,
            // A source the move deleted reports revision 0.
            from_revision: revision(from_revision.unwrap_or_default()),
            to_revision: revision(*to_revision),
        },
        E::ResourceCreated { key, session, kind } => frame::AgentEvent::ResourceCreated {
            resource: resource_key(*key)?,
            session: session_key(*session)?,
            kind: crate::id_bridge::wire_kind(*kind),
        },
        E::ResourceRenamed {
            key,
            name,
            revision: r,
        } => frame::AgentEvent::ResourceRenamed {
            resource: resource_key(*key)?,
            name: name.clone(),
            revision: revision(*r),
        },
        E::ResourceRemoved { key, session, kind } => frame::AgentEvent::ResourceRemoved {
            resource: resource_key(*key)?,
            session: session_key(*session)?,
            kind: crate::id_bridge::wire_kind(*kind),
        },
        E::ActivationChanged {
            resource,
            activation,
            state,
            outcome: o,
        } => frame::AgentEvent::ActivationChanged {
            resource: resource_key(*resource)?,
            activation: activation_id(*activation),
            state: exec_state(*state),
            outcome: o.map(outcome),
        },
        E::ArrangementChanged {
            key,
            owner: o,
            revision: r,
        } => frame::AgentEvent::ArrangementChanged {
            arrangement: arrangement_key(*key)?,
            owner: owner(*o)?,
            revision: r.map(revision),
        },
    })
}
