//! Organization events: field-tagged bodies under their event tags.

use super::{
    AgentEvent, DecodeError, EVENT_TAG_ACTIVATION_CHANGED, EVENT_TAG_ARRANGEMENT_CHANGED,
    EVENT_TAG_INTAKE_CHANGED, EVENT_TAG_LEASE_CHANGED, EVENT_TAG_MEMBERSHIP_CHANGED,
    EVENT_TAG_RESOURCE_CREATED, EVENT_TAG_RESOURCE_REMOVED, EVENT_TAG_RESOURCE_RENAMED,
    EVENT_TAG_SESSION_CREATED, EVENT_TAG_SESSION_DELETED, EVENT_TAG_SESSION_LIFETIME_CHANGED,
    EVENT_TAG_SESSION_RENAMED, Encoder, Fields, encode_outcome, field, req, write_actor, write_key,
    write_name, write_owner, write_project, write_resource, write_session, write_u8, write_u64,
};

/// Write an organization event's field-tagged body and return its tag, or
/// `None` for an event that is not an organization event.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per organization event, kept as one flat encode table"
)]
pub(in crate::wire) fn encode_org_event(event: &AgentEvent, enc: &mut Encoder<'_>) -> Option<u8> {
    let tag = match event {
        AgentEvent::SessionCreated {
            session,
            name,
            lifetime,
            revision,
        } => {
            write_session(enc, field::org::SESSION, session);
            write_name(enc, name);
            write_u8(enc, field::org::LIFETIME, lifetime.to_u8());
            write_u64(enc, field::org::REVISION, revision.get());
            EVENT_TAG_SESSION_CREATED
        }
        AgentEvent::SessionRenamed {
            session,
            name,
            revision,
        } => {
            write_session(enc, field::org::SESSION, session);
            write_name(enc, name);
            write_u64(enc, field::org::REVISION, revision.get());
            EVENT_TAG_SESSION_RENAMED
        }
        AgentEvent::SessionProjectSet {
            session,
            project,
            revision,
        } => {
            write_session(enc, field::org::SESSION, session);
            write_project(enc, project.as_deref());
            write_u64(enc, field::org::REVISION, revision.get());
            crate::wire::frame::EVENT_TAG_SESSION_PROJECT_SET
        }
        AgentEvent::SessionDeleted {
            session,
            name,
            revision,
        } => {
            write_session(enc, field::org::SESSION, session);
            write_name(enc, name);
            write_u64(enc, field::org::REVISION, revision.get());
            EVENT_TAG_SESSION_DELETED
        }
        AgentEvent::SessionLifetimeChanged {
            session,
            lifetime,
            revision,
        } => {
            write_session(enc, field::org::SESSION, session);
            write_u8(enc, field::org::LIFETIME, lifetime.to_u8());
            write_u64(enc, field::org::REVISION, revision.get());
            EVENT_TAG_SESSION_LIFETIME_CHANGED
        }
        AgentEvent::MembershipChanged {
            resource,
            from,
            to,
            from_revision,
            to_revision,
        } => {
            write_resource(enc, resource);
            write_session(enc, field::org::FROM, from);
            write_u64(enc, field::org::FROM_REVISION, from_revision.get());
            write_session(enc, field::org::TO, to);
            write_u64(enc, field::org::TO_REVISION, to_revision.get());
            EVENT_TAG_MEMBERSHIP_CHANGED
        }
        AgentEvent::ResourceCreated {
            resource,
            session,
            kind,
        } => {
            write_session(enc, field::org::SESSION, session);
            write_resource(enc, resource);
            write_u8(enc, field::org::KIND, kind.as_wire());
            EVENT_TAG_RESOURCE_CREATED
        }
        AgentEvent::ResourceRemoved {
            resource,
            session,
            kind,
        } => {
            write_session(enc, field::org::SESSION, session);
            write_resource(enc, resource);
            write_u8(enc, field::org::KIND, kind.as_wire());
            EVENT_TAG_RESOURCE_REMOVED
        }
        AgentEvent::ActivationChanged {
            resource,
            activation,
            state,
            outcome,
        } => {
            write_resource(enc, resource);
            write_u64(enc, field::org::ACTIVATION, activation.get());
            write_u8(enc, field::org::STATE, state.to_u8());
            if let Some(outcome) = outcome {
                enc.write_field_with(field::org::OUTCOME, |e| encode_outcome(*outcome, e));
            }
            EVENT_TAG_ACTIVATION_CHANGED
        }
        AgentEvent::ArrangementChanged {
            arrangement,
            owner,
            revision,
        } => {
            write_owner(enc, *owner);
            write_key(enc, field::org::ARRANGEMENT, arrangement.as_bytes());
            if let Some(revision) = revision {
                write_u64(enc, field::org::REVISION, revision.get());
            }
            EVENT_TAG_ARRANGEMENT_CHANGED
        }
        AgentEvent::ResourceRenamed {
            resource,
            name,
            revision,
        } => {
            write_resource(enc, resource);
            if let Some(name) = name {
                write_name(enc, name);
            }
            write_u64(enc, field::org::REVISION, revision.get());
            EVENT_TAG_RESOURCE_RENAMED
        }
        AgentEvent::LeaseChanged {
            resource,
            epoch,
            holder,
        } => {
            write_resource(enc, resource);
            write_u64(enc, field::org::EPOCH, epoch.get());
            if let Some(holder) = holder {
                write_actor(enc, field::org::HOLDER, holder);
            }
            EVENT_TAG_LEASE_CHANGED
        }
        AgentEvent::IntakeChanged {
            session,
            awaiting_organization,
            revision,
        } => {
            write_session(enc, field::org::SESSION, session);
            write_u64(enc, field::org::REVISION, revision.get());
            write_u8(enc, field::org::AWAITING, u8::from(*awaiting_organization));
            EVENT_TAG_INTAKE_CHANGED
        }
        _ => return None,
    };
    Some(tag)
}

/// Decode an organization event body for `tag`. `None` for a tag that is
/// not an organization event, or a body that is malformed or names an
/// unknown enum value; the caller keeps either as [`AgentEvent::Unknown`].
pub(in crate::wire) fn decode_org_event(tag: u8, body: &[u8]) -> Option<AgentEvent> {
    ((EVENT_TAG_SESSION_CREATED..=EVENT_TAG_RESOURCE_RENAMED).contains(&tag)
        || tag == crate::wire::frame::EVENT_TAG_SESSION_PROJECT_SET)
        .then(|| org_event(tag, body).ok())
        .flatten()
}

pub(super) fn org_event(tag: u8, body: &[u8]) -> Result<AgentEvent, DecodeError> {
    let f = Fields::read(body)?;
    let event = match tag {
        EVENT_TAG_SESSION_CREATED => AgentEvent::SessionCreated {
            session: f.session()?,
            lifetime: f.lifetime()?.unwrap_or_default(),
            revision: req(f.revision)?,
            name: req(f.name)?,
        },
        EVENT_TAG_SESSION_RENAMED => AgentEvent::SessionRenamed {
            session: f.session()?,
            revision: req(f.revision)?,
            name: req(f.name)?,
        },
        crate::wire::frame::EVENT_TAG_SESSION_PROJECT_SET => AgentEvent::SessionProjectSet {
            session: f.session()?,
            revision: req(f.revision)?,
            project: f.project,
        },
        EVENT_TAG_SESSION_DELETED => AgentEvent::SessionDeleted {
            session: f.session()?,
            revision: req(f.revision)?,
            name: req(f.name)?,
        },
        EVENT_TAG_SESSION_LIFETIME_CHANGED => AgentEvent::SessionLifetimeChanged {
            session: f.session()?,
            lifetime: req(f.lifetime()?)?,
            revision: req(f.revision)?,
        },
        EVENT_TAG_MEMBERSHIP_CHANGED => AgentEvent::MembershipChanged {
            resource: f.resource()?,
            from: req(f.from)?,
            to: req(f.to)?,
            from_revision: req(f.from_revision)?,
            to_revision: req(f.to_revision)?,
        },
        EVENT_TAG_RESOURCE_CREATED => AgentEvent::ResourceCreated {
            resource: f.resource()?,
            session: f.session()?,
            kind: f.kind.unwrap_or_default(),
        },
        EVENT_TAG_RESOURCE_REMOVED => AgentEvent::ResourceRemoved {
            resource: f.resource()?,
            session: f.session()?,
            kind: f.kind.unwrap_or_default(),
        },
        EVENT_TAG_ACTIVATION_CHANGED => AgentEvent::ActivationChanged {
            resource: f.resource()?,
            state: req(f.state()?)?,
            activation: req(f.activation)?,
            outcome: f.outcome,
        },
        EVENT_TAG_ARRANGEMENT_CHANGED => AgentEvent::ArrangementChanged {
            arrangement: req(f.arrangement)?,
            owner: f.owner()?,
            revision: f.revision,
        },
        EVENT_TAG_RESOURCE_RENAMED => AgentEvent::ResourceRenamed {
            resource: f.resource()?,
            revision: req(f.revision)?,
            name: f.name,
        },
        EVENT_TAG_LEASE_CHANGED => AgentEvent::LeaseChanged {
            resource: f.resource()?,
            epoch: req(f.epoch)?,
            holder: f.holder,
        },
        EVENT_TAG_INTAKE_CHANGED => AgentEvent::IntakeChanged {
            session: f.session()?,
            awaiting_organization: req(f.awaiting)?,
            revision: req(f.revision)?,
        },
        _ => return Err(DecodeError::unknown_enum("AgentEvent", tag)),
    };
    Ok(event)
}
