//! Which Terminals each snapshot view last listed, so a close is proven
//! only against the same view's previous listing.

use std::collections::HashSet;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::info::SessionSnapshot;

use super::terminal_resources;

/// Which answer a snapshot is. Each is a view filtered by its own grant verb
/// (`ATTACHED` by `OBSERVE`, `GET_STATE` by `INVENTORY`),
/// and only a hub's `GET_STATE` lists satellite terminals.
#[derive(Debug, Clone, Copy)]
pub(in crate::control) enum View {
    Attach,
    Inventory,
}

/// The Terminals each view last listed. A snapshot is a view, not a death
/// certificate: absence proves a close only against the same view's
/// previous listing, and `RESOURCE_CLOSED` is the authority otherwise.
/// Kept across reconnects, so a close missed while disconnected is found.
#[derive(Debug, Default)]
pub(in crate::control) struct Listings {
    attach: HashSet<ResourceId>,
    inventory: HashSet<ResourceId>,
}

impl Listings {
    /// Record `snapshot` as `view`'s listing and return the Terminals it
    /// proves closed: listed by that view before and absent now. A satellite
    /// terminal is proven closed only while its hub reports the host
    /// reachable; until then it stays in the listing, unknown.
    pub(in crate::control) fn relist(
        &mut self,
        view: View,
        snapshot: &SessionSnapshot,
    ) -> Vec<ResourceId> {
        let now: HashSet<ResourceId> = terminal_resources(snapshot)
            .map(|pane| pane.id.clone())
            .collect();
        let previous = std::mem::replace(self.listing(view), now);
        let mut closed = Vec::new();
        for id in previous {
            if self.listing(view).contains(&id) {
                continue;
            }
            if unlisted_host(snapshot, &id) {
                self.listing(view).insert(id);
            } else {
                closed.push(id);
            }
        }
        closed
    }

    /// Whether `snapshot`, as `view`, omits a Terminal the other view lists.
    pub(in crate::control) fn hides(&self, view: View, snapshot: &SessionSnapshot) -> bool {
        let other = match view {
            View::Attach => &self.inventory,
            View::Inventory => &self.attach,
        };
        other
            .iter()
            .any(|id| !terminal_resources(snapshot).any(|pane| &pane.id == id))
    }

    /// A close already applied needs no proof from either view.
    pub(in crate::control) fn forget(&mut self, id: &ResourceId) {
        self.attach.remove(id);
        self.inventory.remove(id);
    }

    const fn listing(&mut self, view: View) -> &mut HashSet<ResourceId> {
        match view {
            View::Attach => &mut self.attach,
            View::Inventory => &mut self.inventory,
        }
    }
}

/// A satellite terminal whose host the snapshot does not report reachable:
/// its absence says nothing about the terminal (federation degradation).
fn unlisted_host(snapshot: &SessionSnapshot, id: &ResourceId) -> bool {
    let ResourceId::Satellite { host, .. } = id else {
        return false;
    };
    !snapshot
        .hosts()
        .iter()
        .any(|row| &row.host == host && row.is_reachable())
}
