//! Server-correlated membership edits. A submitted move is not a completed move.

use std::sync::Arc;
use std::time::{Duration, Instant};

use phux_client_core::layout::{ArrangementDoc, adopt_unarranged_groups, leaves};
use phux_client_core::org_ops::OrgOutcome;
use phux_client_runtime::{Client, control::Status};
use phux_protocol::ids::{ArrangementKey, OperationId, ResourceId, Revision, SessionKey};

use crate::desk::{Desk, NoticeKind};

pub(super) struct Move {
    follow: Option<(ResourceId, SessionKey)>,
    placement: Placement,
    choice: u64,
}

pub(super) struct Placement {
    to: SessionKey,
    fallback: Arc<ArrangementDoc>,
    since: Instant,
}

pub(super) struct Placing {
    operation: OperationId,
    session: SessionKey,
    arrangement: ArrangementKey,
    base: Revision,
    since: Instant,
}

const PLACEMENT_WAIT: Duration = Duration::from_secs(3);

impl Desk {
    /// The client, once it is attached and can carry an organization write.
    pub(in crate::desk) fn attached_client(&self) -> Option<&Arc<Client>> {
        self.connection
            .client
            .as_ref()
            .filter(|client| client.status() == Status::Attached)
    }

    /// Whether the keyboard still follows `live`: it is shown, and nothing
    /// else has been asked for since. A move only carries the view along
    /// when the person has not navigated away while it travelled.
    fn follows(&self, live: &ResourceId) -> bool {
        let shown = self.connection.shown.as_ref();
        shown.is_some_and(|s| s.resource == *live)
            && (self.connection.wanted.as_ref()).is_none_or(|(wanted, _)| wanted == live)
    }

    pub(super) fn move_sidebar_resource(&mut self, id: u32, from: SessionKey, to: SessionKey) {
        let Some(client) = self.attached_client().cloned() else {
            return;
        };
        let live = ResourceId::local(id);
        let organization = client.organization();
        let Some(resource) = organization.resource_by_live(&live) else {
            return;
        };
        if resource.session != from || from == to {
            return;
        }
        match client.move_resource(resource.key, to) {
            Ok(operation) => {
                let follow = self.follows(&live).then(|| (live.clone(), to));
                let placement = Placement {
                    to,
                    fallback: Arc::new(ArrangementDoc::single(live.clone())),
                    since: Instant::now(),
                };
                self.connection.reorder.moves.insert(
                    operation,
                    Move {
                        follow,
                        placement,
                        choice: self.connection.view_choice,
                    },
                );
            }
            Err(error) => self.say(NoticeKind::Error, format!("Resource not moved: {error}")),
        }
    }

    pub(super) fn move_sidebar_tab(&mut self, id: [u8; 16], to: SessionKey) {
        let Some(client) = self.attached_client().cloned() else {
            return;
        };
        let Some(tab) = self.connection.tabs.list.iter().find(|tab| tab.id == id) else {
            return;
        };
        let organization = client.organization();
        // No satellite, missing or already-moved member may silently turn a
        // whole-tab drop into a move of only part of that tab.
        let resources = tab
            .resources
            .iter()
            .map(|live| {
                live.local_id()?;
                let resource = organization.resource_by_live(live)?;
                (resource.session != to).then_some(resource.key)
            })
            .collect::<Option<Vec<_>>>();
        let Some(resources) = resources.filter(|resources| !resources.is_empty()) else {
            return;
        };
        let mut follow = self
            .connection
            .shown
            .as_ref()
            .filter(|s| tab.resources.contains(&s.resource) && self.follows(&s.resource))
            .map(|s| (s.resource.clone(), to));
        let mut fallback = ArrangementDoc::default();
        let source_group = tab
            .resources
            .first()
            .and_then(|live| organization.resource_by_live(live))
            .and_then(|record| organization.arrangement_document(&record.session))
            .and_then(|(_, _, doc, _)| doc.groups.into_iter().find(|group| group.id == id))
            .filter(|group| {
                group.state.tree.as_ref().is_some_and(|tree| {
                    leaves(tree).iter().all(|live| tab.resources.contains(live))
                })
            });
        if let Some(group) = source_group {
            fallback.groups.push(group);
        } else {
            for live in &tab.resources {
                fallback.add_group(fallback.default_group_name(), live.clone());
            }
        }
        let fallback = Arc::new(fallback);
        match client.move_resources(&resources, to) {
            Ok(operations) => {
                let last = operations.last().copied();
                for operation in operations {
                    self.connection.reorder.moves.insert(
                        operation,
                        Move {
                            follow: if Some(operation) == last {
                                follow.take()
                            } else {
                                None
                            },
                            placement: Placement {
                                to,
                                fallback: fallback.clone(),
                                since: Instant::now(),
                            },
                            choice: self.connection.view_choice,
                        },
                    );
                }
            }
            Err(error) => self.say(NoticeKind::Error, format!("Tab not moved: {error}")),
        }
    }

    pub(in crate::desk) fn owns_sidebar_move(&self, operation: OperationId) -> bool {
        self.connection.reorder.moves.contains_key(&operation)
            || self
                .connection
                .reorder
                .placing
                .as_ref()
                .is_some_and(|write| write.operation == operation)
    }

    pub(in crate::desk) fn sidebar_move_outcome(
        &mut self,
        client: &Client,
        operation: OperationId,
        outcome: &OrgOutcome,
    ) -> bool {
        if self
            .connection
            .reorder
            .placing
            .as_ref()
            .is_some_and(|write| write.operation == operation)
        {
            if matches!(outcome, OrgOutcome::Committed(_)) {
                let arrangement = self
                    .connection
                    .reorder
                    .placing
                    .as_ref()
                    .expect("matched placement")
                    .arrangement;
                let _ = client.fetch_arrangement(arrangement);
            } else {
                self.connection.reorder.placing = None;
            }
            return true;
        }
        let Some(pending) = self.connection.reorder.moves.remove(&operation) else {
            return false;
        };
        if !matches!(outcome, OrgOutcome::Committed(_)) {
            self.say(NoticeKind::Error, format!(
                "Resource move operation {} did not settle: {outcome:?}. Check the original operation and the server's resources before retrying.",
                phux_protocol::ids::hex16(operation.as_bytes())
            ));
            return true;
        }
        // Journal delivery and the command receipt may arrive in either order.
        // Refresh inventory rather than making an optimistic ownership edit.
        let _ = client.refresh_organization();
        client.with_control(|control| {
            let _ = control.refresh_topology();
        });
        let mut placement = pending.placement;
        placement.since = Instant::now();
        self.connection.reorder.placements.push(placement);
        if let Some((live, to)) = pending.follow
            && self.follows(&live)
            && pending.choice == self.connection.view_choice
            && self
                .connection
                .view_intents
                .get(&live)
                .is_some_and(|intent| intent.select_session(client, &to))
        {
            // Keep the selected moved resource visible, but do not steal focus
            // if the person navigated elsewhere while the receipt travelled.
            self.connection.zoomed = None;
            self.want_automatic(live);
        }
        true
    }

    pub(in crate::desk) fn cancel_sidebar_moves(&mut self) -> bool {
        if self.connection.reorder.moves.is_empty()
            && self.connection.reorder.placements.is_empty()
            && self.connection.reorder.placing.is_none()
        {
            return false;
        }
        self.connection.reorder.moves.clear();
        self.connection.reorder.placements.clear();
        if let Some(write) = self.connection.reorder.placing.take() {
            self.connection.layout_ops.remove(&write.operation);
        }
        self.say(NoticeKind::Error,
            "Resource moves did not settle: the server incarnation could not be verified. Check the server's resources before retrying.");
        true
    }

    /// One bounded placement phase after membership settles. Never attaches
    /// an unselected destination and never resubmits a refused write.
    pub(super) fn place_sidebar_moves(&mut self, client: &Client) -> bool {
        if self.connection.receipts.waiting()
            || client.status() != Status::Attached
            || (self.connection.reorder.placements.is_empty()
                && self.connection.reorder.placing.is_none())
        {
            return false;
        }
        let organization = client.organization();
        if let Some(write) = &self.connection.reorder.placing {
            let observed = organization
                .arrangement_document(&write.session)
                .is_some_and(|(_, revision, _, _)| revision != write.base);
            if observed && !self.connection.layout_ops.contains(&write.operation) {
                self.connection.reorder.placing = None;
            } else if write.since.elapsed() >= PLACEMENT_WAIT {
                self.connection.reorder.placing = None;
                self.say(NoticeKind::Error, "Resources moved, but their destination tab layout did not settle. Check the destination before retrying.");
                return true;
            } else {
                return false;
            }
        }
        while !self.connection.reorder.placements.is_empty() {
            let pending = &self.connection.reorder.placements[0];
            if pending.since.elapsed() >= PLACEMENT_WAIT {
                self.connection.reorder.placements.remove(0);
                self.say(NoticeKind::Error, "Resources moved, but their destination tab layout could not be loaded. Open them from the sidebar.");
                return true;
            }
            if !organization.is_live() {
                return false;
            }
            let belongs = |live: &ResourceId| {
                organization
                    .resource_by_live(live)
                    .is_some_and(|record| record.session == pending.to)
            };
            if !pending
                .fallback
                .groups
                .iter()
                .flat_map(|group| group.state.tree.iter().flat_map(leaves))
                .any(|live| belongs(&live))
            {
                return false;
            }
            let Some((arrangement, base, mut document, _)) =
                organization.arrangement_document(&pending.to)
            else {
                if let Some(arrangement) = organization
                    .session(&pending.to)
                    .and_then(|s| s.arrangement)
                    && self.connection.fetched.insert(arrangement)
                {
                    let _ = client.fetch_arrangement(arrangement);
                }
                return false;
            };
            let changed = merge_placement(&mut document, &pending.fallback, belongs);
            let pending = self.connection.reorder.placements.remove(0);
            if !changed {
                continue;
            }
            match self.submit_arrangement(client, pending.to, &document) {
                Ok(operation) => {
                    self.connection.reorder.placing = Some(Placing {
                        operation,
                        session: pending.to,
                        arrangement,
                        base,
                        since: Instant::now(),
                    })
                }
                Err(error) => self.say(
                    NoticeKind::Error,
                    format!("Resources moved, but their destination tabs were not saved: {error}"),
                ),
            }
            return true;
        }
        false
    }
}

/// Preserve a whole moved tab when possible. A partially committed bulk
/// move still gets a tab for every member that actually reached the target.
fn merge_placement(
    document: &mut ArrangementDoc,
    fallback: &ArrangementDoc,
    belongs: impl Fn(&ResourceId) -> bool,
) -> bool {
    let before = document.groups.len();
    let active = document.active;
    if fallback
        .groups
        .iter()
        .flat_map(|group| group.state.tree.iter().flat_map(leaves))
        .all(|live| belongs(&live))
    {
        adopt_unarranged_groups(document, fallback);
    }
    for live in fallback
        .groups
        .iter()
        .flat_map(|group| group.state.tree.iter().flat_map(leaves))
    {
        if belongs(&live)
            && !document.groups.iter().any(|group| {
                group
                    .state
                    .tree
                    .as_ref()
                    .is_some_and(|tree| leaves(tree).contains(&live))
            })
        {
            document.add_group(document.default_group_name(), live);
        }
    }
    document.active = active.min(document.groups.len().saturating_sub(1));
    document.groups.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_is_idempotent_and_preserves_the_destinations_active_group() {
        let mut target = ArrangementDoc::single(ResourceId::local(1));
        let fallback = ArrangementDoc::single(ResourceId::local(2));
        assert!(merge_placement(&mut target, &fallback, |_| true));
        assert_eq!(target.active, 0);
        assert!(!merge_placement(&mut target, &fallback, |_| true));
        assert_eq!(target.groups.len(), 2);
    }

    #[test]
    fn partially_committed_tab_moves_place_only_members_that_reached_the_destination() {
        let mut fallback = ArrangementDoc::single(ResourceId::local(1));
        fallback.add_group("2".into(), ResourceId::local(2));
        let mut target = ArrangementDoc::default();
        assert!(merge_placement(&mut target, &fallback, |id| *id == ResourceId::local(1)));
        assert_eq!(target.groups.len(), 1);
        assert_eq!(
            leaves(target.groups[0].state.tree.as_ref().unwrap()),
            vec![ResourceId::local(1)]
        );
        assert!(merge_placement(&mut target, &fallback, |_| true));
        assert_eq!(target.groups.len(), 2);
    }
}
