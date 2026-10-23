//! Events: extracted from the shared module.
use super::{
    activation_id, arrangement_key, exec_state, frame, lifetime, outcome, owner, resource_key,
    revision, session_key, wire,
};

/// A core actor as the event stamp's `ActorRef`, with its principal.
#[must_use]
pub fn actor_ref(actor: &phux_core::Actor, client: Option<wire::ClientId>) -> frame::ActorRef {
    frame::ActorRef::new(client.unwrap_or(wire::ClientId::new(0)))
        .with_client_name(actor.client.clone())
        .with_principal(principal_id(&actor.principal))
        .with_on_behalf_of(actor.on_behalf_of.as_ref().and_then(principal_id))
}

/// A principal's stable wire id: the first 16 bytes of the SHA-256 of its
/// label (`owner`, `system`, or the credential name).
#[must_use]
pub fn principal_id(principal: &phux_core::PrincipalId) -> Option<wire::PrincipalId> {
    match principal {
        phux_core::PrincipalId::Owner => digest_id(b"owner"),
        phux_core::PrincipalId::System => digest_id(b"system"),
        phux_core::PrincipalId::Credential(name) => digest_id(name.as_bytes()),
    }
}

pub(super) fn digest_id(label: &[u8]) -> Option<wire::PrincipalId> {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(label);
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    wire::PrincipalId::new(bytes)
}

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
            kind: crate::resource::wire_kind(*kind),
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
            kind: crate::resource::wire_kind(*kind),
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
