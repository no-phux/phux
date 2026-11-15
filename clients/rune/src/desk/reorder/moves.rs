//! Server-correlated membership edits. A submitted move is not a completed move.

use phux_client_core::org_ops::OrgOutcome;
use phux_client_runtime::{Client, control::Status};
use phux_protocol::ids::{OperationId, ResourceId, SessionId, SessionKey};
use phux_protocol::wire::frame::AttachTarget;

use crate::desk::{Desk, NoticeKind};

pub(super) struct Move {
    follow: Option<(ResourceId, SessionKey)>,
}

impl Desk {
    pub(super) fn move_sidebar_resource(&mut self, id: u32, from: SessionKey, to: SessionKey) {
        let Some(client) = self.client.clone().filter(|c| c.status() == Status::Attached) else {
            return;
        };
        let live = ResourceId::local(id);
        let organization = client.organization();
        let Some(resource) = organization.resource_by_live(&live) else { return };
        if resource.session != from || from == to {
            return;
        }
        match client.move_resource(resource.key, to) {
            Ok(operation) => {
                let follow = self.shown.as_ref().filter(|s| s.resource == live)
                    .map(|_| (live, to));
                self.reorder.moves.insert(operation, Move { follow });
            }
            Err(error) => self.say(NoticeKind::Error, format!("Resource not moved: {error}")),
        }
    }

    pub(super) fn move_sidebar_tab(&mut self, id: [u8; 16], to: SessionKey) {
        let Some(client) = self.client.clone().filter(|c| c.status() == Status::Attached) else {
            return;
        };
        let Some(tab) = self.tabs.list.iter().find(|tab| tab.id == id) else { return };
        let organization = client.organization();
        // No satellite, missing or already-moved member may silently turn a
        // whole-tab drop into a move of only part of that tab.
        let resources = tab.resources.iter().map(|live| {
            live.local_id()?;
            let resource = organization.resource_by_live(live)?;
            (resource.session != to).then_some(resource.key)
        }).collect::<Option<Vec<_>>>();
        let Some(resources) = resources.filter(|resources| !resources.is_empty()) else { return };
        let mut follow = self.shown.as_ref().filter(|s| tab.resources.contains(&s.resource))
            .map(|s| (s.resource.clone(), to));
        match client.move_resources(&resources, to) {
            Ok(operations) => {
                let last = operations.last().copied();
                for operation in operations {
                    self.reorder.moves.insert(operation, Move {
                        follow: if Some(operation) == last { follow.take() } else { None },
                    });
                }
            }
            Err(error) => self.say(NoticeKind::Error, format!("Tab not moved: {error}")),
        }
    }

    pub(in crate::desk) fn owns_sidebar_move(&self, operation: OperationId) -> bool {
        self.reorder.moves.contains_key(&operation)
    }

    pub(in crate::desk) fn sidebar_move_outcome(
        &mut self,
        client: &Client,
        operation: OperationId,
        outcome: &OrgOutcome,
    ) -> bool {
        let Some(pending) = self.reorder.moves.remove(&operation) else { return false };
        if !outcome.succeeded() {
            self.say(NoticeKind::Error, format!(
                "Resource move did not settle: {outcome:?}. Check the server's resources before retrying."
            ));
            return true;
        }
        // Journal delivery and the command receipt may arrive in either order.
        // Refresh inventory rather than making an optimistic ownership edit.
        let _ = client.refresh_organization();
        client.with_control(|control| { let _ = control.refresh_topology(); });
        if let Some((live, to)) = pending.follow
            && self.shown.as_ref().is_some_and(|s| s.resource == live)
            && let Some(session) = self.topology.as_ref().and_then(|t| t.session_keyed(&to))
        {
            // Keep the selected moved resource visible, but do not steal focus
            // if the person navigated elsewhere while the receipt travelled.
            client.attach_session(AttachTarget::ById(SessionId::new(session.id)));
            self.zoomed = None;
            self.want(live);
        }
        true
    }

    pub(in crate::desk) fn cancel_sidebar_moves(&mut self) -> bool {
        if self.reorder.moves.is_empty() { return false; }
        self.reorder.moves.clear();
        self.say(NoticeKind::Error,
            "Resource moves did not settle: the server incarnation could not be verified. Check the server's resources before retrying.");
        true
    }
}
