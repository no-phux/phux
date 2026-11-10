//! Every resource of a snapshot, of any kind, in the server's order: the
//! primary listing the per-kind views are cut from.

use phux_protocol::ids::{ResourceId, ResourceKind, SessionId};
use phux_protocol::wire::info::SessionSnapshot;

use super::{DocumentDescriptor, OrganizationReplica, session_of};

/// One resource of any kind, denormalized with its session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceDescriptor {
    /// The resource.
    pub id: ResourceId,
    /// Its kind; an unknown wire kind is kept, not dropped.
    pub kind: ResourceKind,
    /// The live session it belongs to; 0 when none is named.
    pub session_id: u32,
    /// The resource it is bound to: an agent session's or a document's
    /// producer pane.
    pub parent: Option<ResourceId>,
    /// A Terminal's user-set title, else its program's; a document's
    /// declared name.
    pub title: Option<String>,
    /// A Terminal's grid as `(cols, rows)`; `None` for every other kind.
    pub cells: Option<(u16, u16)>,
}

/// The snapshot's resources, every kind, in its order.
pub(super) fn resources(snapshot: &SessionSnapshot) -> Vec<ResourceDescriptor> {
    snapshot
        .resources
        .iter()
        .map(|resource| {
            let terminal = resource.kind.is_terminal();
            let title = if terminal {
                resource
                    .title
                    .clone()
                    .or_else(|| resource.program_title.clone())
            } else {
                resource.title.clone()
            };
            ResourceDescriptor {
                id: resource.id.clone(),
                kind: resource.kind,
                session_id: session_of(snapshot, resource).map_or(0, SessionId::get),
                parent: resource.parent.clone(),
                title,
                cells: terminal.then_some((resource.cols, resource.rows)),
            }
        })
        .collect()
}

/// The document-kind view of `resources`.
pub(super) fn documents(resources: &[ResourceDescriptor]) -> Vec<DocumentDescriptor> {
    resources
        .iter()
        .filter(|resource| resource.kind.is_document())
        .map(|resource| DocumentDescriptor {
            id: resource.id.clone(),
            kind: resource.kind,
            parent: resource.parent.clone(),
            name: resource.title.clone(),
        })
        .collect()
}

/// Move each resource the replica records into its record's live session.
pub(super) fn rehome(resources: &mut [ResourceDescriptor], replica: &OrganizationReplica) {
    for resource in resources {
        let live = replica
            .resource_by_live(&resource.id)
            .and_then(|record| replica.session(&record.session))
            .and_then(|session| session.live);
        if let Some(live) = live {
            resource.session_id = live.get();
        }
    }
}

#[cfg(test)]
mod tests {
    use phux_protocol::ids::{ResourceId, ResourceKind, SessionId, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot};

    use super::super::Topology;
    use super::ResourceDescriptor;

    /// One resource of every kind, in a deliberately unsorted order.
    fn mixed() -> SessionSnapshot {
        let session = SessionId::new(1);
        let resource = |id, kind| {
            ResourceInfo::new(ResourceId::local(id), WindowId::new(0), 0, 0)
                .with_kind(kind)
                .with_session(Some(session))
        };
        SessionSnapshot::new(session, WindowId::new(1), ResourceId::local(1))
            .with_sessions(vec![SessionInfo::new(session, "work")])
            .with_resources(vec![
                resource(5, ResourceKind::Document)
                    .with_parent(Some(ResourceId::local(1)))
                    .with_title(Some("notes".to_owned())),
                ResourceInfo::new(ResourceId::local(1), WindowId::new(1), 80, 24)
                    .with_session(Some(session))
                    .with_program_title(Some("vim".to_owned())),
                resource(4, ResourceKind::AgentSession).with_parent(Some(ResourceId::local(1))),
                resource(3, ResourceKind::PixelSurface).with_parent(Some(ResourceId::local(1))),
                resource(2, ResourceKind::AccessibilityTree),
                resource(9, ResourceKind::Unknown { tag: 90 }),
            ])
    }

    #[test]
    fn resources_list_every_kind_in_server_order() {
        let topology = Topology::from_snapshot(&mixed());
        let listed: Vec<_> = topology
            .resources
            .iter()
            .map(|resource| (resource.id.clone(), resource.kind))
            .collect();
        assert_eq!(
            listed,
            [
                (ResourceId::local(5), ResourceKind::Document),
                (ResourceId::local(1), ResourceKind::Terminal),
                (ResourceId::local(4), ResourceKind::AgentSession),
                (ResourceId::local(3), ResourceKind::PixelSurface),
                (ResourceId::local(2), ResourceKind::AccessibilityTree),
                (ResourceId::local(9), ResourceKind::Unknown { tag: 90 }),
            ]
        );
        assert_eq!(
            topology.resources[1],
            ResourceDescriptor {
                id: ResourceId::local(1),
                kind: ResourceKind::Terminal,
                session_id: 1,
                parent: None,
                title: Some("vim".to_owned()),
                cells: Some((80, 24)),
            }
        );
        let document = &topology.resources[0];
        assert_eq!(
            (
                document.session_id,
                document.parent.clone(),
                document.title.as_deref(),
                document.cells
            ),
            (1, Some(ResourceId::local(1)), Some("notes"), None)
        );
    }

    #[test]
    fn per_kind_views_are_cut_from_the_resource_listing() {
        let topology = Topology::from_snapshot(&mixed());
        let documents: Vec<_> = topology.documents.iter().map(|d| d.id.clone()).collect();
        assert_eq!(
            documents,
            [
                ResourceId::local(5),
                ResourceId::local(3),
                ResourceId::local(2)
            ]
        );
        assert_eq!(topology.documents[0].name.as_deref(), Some("notes"));
        assert_eq!(topology.panes.len(), 1);
        assert_eq!(topology.agent_sessions.len(), 1);
        assert_eq!(topology.agent_sessions[0].id, ResourceId::local(4));
    }
}
