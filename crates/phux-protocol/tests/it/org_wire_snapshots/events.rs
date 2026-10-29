//! One golden fixture per organization event.

use super::{arrangement, event, holder, other_session, resource, rev, session};
use phux_protocol::ids::{ActivationId, LeaseEpoch, PrincipalId, ResourceKind, WorkspaceKey};
use phux_protocol::wire::frame::{
    AgentEvent, ArrangementOwner, ExecState, FrameKind, Outcome, SessionLifetime,
};

#[allow(
    clippy::too_many_lines,
    reason = "one fixture per organization event, kept as one flat table"
)]
pub(super) fn event_fixtures() -> Vec<(&'static str, FrameKind)> {
    vec![
        (
            "snap_org_event_session_created",
            event(AgentEvent::SessionCreated {
                session: session(),
                name: "work".to_owned(),
                lifetime: SessionLifetime::Persistent,
                revision: rev(1),
            }),
        ),
        (
            "snap_org_event_session_renamed",
            event(AgentEvent::SessionRenamed {
                session: session(),
                name: "play".to_owned(),
                revision: rev(2),
            }),
        ),
        (
            "snap_org_event_session_project_set",
            event(AgentEvent::SessionProjectSet {
                session: session(),
                project: Some("phux".to_owned()),
                revision: rev(2),
            }),
        ),
        (
            "snap_org_event_session_project_cleared",
            event(AgentEvent::SessionProjectSet {
                session: session(),
                project: None,
                revision: rev(3),
            }),
        ),
        (
            "snap_org_event_session_deleted",
            event(AgentEvent::SessionDeleted {
                session: session(),
                name: "play".to_owned(),
                revision: rev(3),
            }),
        ),
        (
            "snap_org_event_session_lifetime_changed",
            event(AgentEvent::SessionLifetimeChanged {
                session: session(),
                lifetime: SessionLifetime::Ephemeral,
                revision: rev(3),
            }),
        ),
        (
            "snap_org_event_membership_changed",
            event(AgentEvent::MembershipChanged {
                resource: resource(),
                from: session(),
                to: other_session(),
                from_revision: rev(5),
                to_revision: rev(8),
            }),
        ),
        (
            "snap_org_event_resource_created",
            event(AgentEvent::ResourceCreated {
                resource: resource(),
                session: session(),
                kind: ResourceKind::Terminal,
            }),
        ),
        (
            "snap_org_event_resource_removed",
            event(AgentEvent::ResourceRemoved {
                resource: resource(),
                session: session(),
                kind: ResourceKind::AgentSession,
            }),
        ),
        (
            "snap_org_event_activation_changed",
            event(AgentEvent::ActivationChanged {
                resource: resource(),
                activation: ActivationId::new(3),
                state: ExecState::Completed,
                outcome: Some(Outcome::Signaled(9)),
            }),
        ),
        (
            "snap_org_event_arrangement_changed",
            event(AgentEvent::ArrangementChanged {
                arrangement: arrangement(),
                owner: ArrangementOwner::Workspace(WorkspaceKey::new([0xB0; 16]).unwrap()),
                revision: Some(rev(2)),
            }),
        ),
        (
            "snap_org_event_arrangement_deleted",
            event(AgentEvent::ArrangementChanged {
                arrangement: arrangement(),
                owner: ArrangementOwner::Session(session()),
                revision: None,
            }),
        ),
        (
            "snap_org_event_lease_changed",
            event(AgentEvent::LeaseChanged {
                resource: resource(),
                epoch: LeaseEpoch::new(6),
                holder: Some(holder().with_on_behalf_of(PrincipalId::new([0x92; 16]))),
            }),
        ),
        (
            "snap_org_event_intake_changed",
            event(AgentEvent::IntakeChanged {
                session: session(),
                awaiting_organization: false,
                revision: rev(6),
            }),
        ),
        (
            "snap_org_event_resource_renamed",
            event(AgentEvent::ResourceRenamed {
                resource: resource(),
                name: Some("logs".to_owned()),
                revision: rev(3),
            }),
        ),
    ]
}
