//! Drag tabs and sessions within their server order, move local resources
//! between sessions, or drop one on a pane of its session to make it a tab of
//! that pane's slot. Only laid-out handles accept a drop; the server owns
//! every resulting order and membership change.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use phux_client_core::layout::leaves;
use phux_client_runtime::Client;
use phux_client_runtime::control::SessionDescriptor;
use phux_client_runtime::session_order::{self, SESSION_ORDER_KEY};
use phux_protocol::ids::{OperationId, ResourceId, SessionKey};
use phux_protocol::wire::frame::{FrameKind, Scope};
use rune::{Drag, Phase};

use super::Desk;
use crate::creature::{self, Stats};

mod moves;

/// What a drag picked up, or is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Grab {
    Tab([u8; 16]),
    /// Local live handle and its session when the gesture started.
    Resource(u32, SessionKey),
    Session(SessionKey),
    /// A terminal pane of the shown layout, as a slot a resource can join.
    Pane(u32),
}

#[derive(Default)]
pub struct Reorder {
    /// The item held and the one under the pointer, while dragging.
    pub held: Option<(Grab, Grab)>,
    /// Where each tab, resource row and session header last laid out.
    rects: HashMap<Grab, [f32; 4]>,
    /// The server's session order, as last heard.
    order: Vec<SessionKey>,
    /// The client the order key is watched on.
    watching: Weak<Client>,
    moves: HashMap<OperationId, moves::Move>,
    placements: Vec<moves::Placement>,
    placing: Option<moves::Placing>,
    /// The scroll viewport, excluding the sidebar's footer and other chrome.
    sidebar: Option<[f32; 4]>,
}

impl Reorder {
    pub fn laid_out(&mut self, grab: Grab, rect: [f32; 4]) {
        self.rects.insert(grab, rect);
    }

    pub fn sidebar_laid_out(&mut self, rect: [f32; 4]) {
        self.sidebar = Some(rect);
    }

    /// The server's session order, as last heard.
    pub fn order(&self) -> &[SessionKey] {
        &self.order
    }

    /// The item a drop would land on, when it is not the one held.
    pub fn target(&self) -> Option<Grab> {
        self.held.and_then(|(from, over)| match (from, over) {
            (Grab::Resource(_, source), Grab::Session(to)) if source == to => None,
            _ => (from != over).then_some(over),
        })
    }
}

/// Both axes matter: gaps, the footer and coordinates outside a handle
/// are not a drop on its nearest neighbour. Right/bottom edges are exclusive.
fn contains([x, y, w, h]: [f32; 4], [px, py]: [f32; 2]) -> bool {
    px >= x && py >= y && px < x + w && py < y + h
}

/// `keys` with `from` moved to where `to` stands in it: a drop's index is
/// in the whole stored order, never within a group.
fn moved(mut keys: Vec<SessionKey>, from: SessionKey, to: SessionKey) -> Option<Vec<SessionKey>> {
    let at = |keys: &[SessionKey], key| keys.iter().position(|k| *k == key);
    let (from, to) = (at(&keys, from)?, at(&keys, to)?);
    let key = keys.remove(from);
    keys.insert(to, key);
    Some(keys)
}

impl Desk {
    /// The sessions as the sidebar lists them: the stored order, then the
    /// rest as the server lists them. The quick terminal's is the panel's.
    pub fn ordered_sessions(&self) -> Vec<&SessionDescriptor> {
        let all = self.connection.topology.iter().flat_map(|t| &t.sessions);
        let listed =
            |s: &&SessionDescriptor| self.quick.is_some() || !super::quick::hidden(&s.name);
        let mut sessions: Vec<_> = all.filter(listed).collect();
        session_order::list(
            &mut sessions,
            &self.connection.reorder.order,
            |s| s.key,
            |s| s.touched,
        );
        sessions
    }

    /// One phase of a drag, always hit-tested in window coordinates.
    pub fn drag_reorder(&mut self, grab: Grab, drag: &Drag) {
        self.drag_at(grab, drag.phase, drag.window_point(), drag.cancelled());
    }

    pub(super) fn drag_at(&mut self, grab: Grab, phase: Phase, point: [f32; 2], cancelled: bool) {
        self.connection.reorder.held = None;
        if phase == Phase::Down || cancelled {
            return;
        }
        let over = self.drop_target(grab, point);
        if phase == Phase::Move {
            self.connection.reorder.held = over.map(|over| (grab, over));
            return;
        }
        match (grab, over) {
            (Grab::Tab(from), Some(Grab::Tab(to))) if from != to => self.move_tab(from, to),
            (Grab::Session(from), Some(Grab::Session(to))) if from != to => {
                self.move_session(from, to);
            }
            (Grab::Resource(id, from), Some(Grab::Session(to))) if from != to => {
                self.move_sidebar_resource(id, from, to);
            }
            (Grab::Tab(id), Some(Grab::Session(to))) => self.move_sidebar_tab(id, to),
            (Grab::Resource(id, _), Some(Grab::Pane(to))) => self.join_slot(id, to),
            _ => {}
        }
    }

    pub(super) fn drop_target(&self, held: Grab, point: [f32; 2]) -> Option<Grab> {
        // Tabs retain their strip's own reorder targets.
        if matches!(held, Grab::Tab(_))
            && let Some(tab) = self.tab_strip_target(point)
        {
            return Some(tab);
        }
        if let Grab::Resource(id, session) = held
            && let Some(pane) = self.slot_drop(id, session, point)
        {
            return Some(Grab::Pane(pane));
        }
        self.sidebar_target(held, point)
    }

    /// Whether the remembered rectangle of `grab` holds `point`.
    fn rect_hit(&self, grab: Grab, point: [f32; 2]) -> bool {
        (self.connection.reorder.rects.get(&grab)).is_some_and(|r| contains(*r, point))
    }

    /// The strip tab under `point`.
    fn tab_strip_target(&self, point: [f32; 2]) -> Option<Grab> {
        (self.connection.tabs.list.iter())
            .map(|tab| Grab::Tab(tab.id))
            .find(|grab| self.rect_hit(*grab, point))
    }

    /// The sidebar session `held` would drop into at `point`. A resource row
    /// is a target for its owning session, never a tab, and only rows and
    /// sessions still listed count.
    fn sidebar_target(&self, held: Grab, point: [f32; 2]) -> Option<Grab> {
        let in_sidebar = self
            .connection
            .reorder
            .sidebar
            .is_some_and(|r| contains(r, point));
        if !self.sidebar_shown() || !in_sidebar {
            return None;
        }
        // A tab cannot move into the session it is already in.
        let selected =
            (self.connection.client.as_ref()).and_then(|client| client.selected_session());
        let sessions = self.connection.topology.iter().flat_map(|t| &t.sessions);
        for session in sessions {
            if self.quick.is_none() && super::quick::hidden(&session.name) {
                continue;
            }
            if matches!(held, Grab::Tab(_)) && selected == Some(session.id) {
                continue;
            }
            let Some(key) = session.key else { continue };
            let grab = Grab::Session(key);
            if self.rect_hit(grab, point) {
                return Some(grab);
            }
            if self.sidebar_collapsed.contains(&key) {
                continue;
            }
            let over_row = self.connection.reorder.rects.iter().any(|(row, rect)| {
                matches!(*row, Grab::Resource(id, owner)
                    if owner == key && self.resource_in_session(id, key, session.id))
                    && contains(*rect, point)
            });
            if over_row {
                return Some(grab);
            }
        }
        None
    }

    /// The pane under `point` the resource `id` would join as a slot tab:
    /// one the shown layout places, of the same session, on a server that
    /// keeps slot tabs.
    fn slot_drop(&self, id: u32, session: SessionKey, point: [f32; 2]) -> Option<u32> {
        let client = self.connection.client.as_ref()?;
        let shown = self.selected_entry(client)?;
        if shown.key != Some(session) || !client.slot_tabs_supported() {
            return None;
        }
        let under = |pane: &ResourceId| {
            let [x, y] = *self.connection.origins.get(pane)?;
            let [w, h] = *self.connection.areas.get(pane)?;
            contains([x, y, w, h], point)
                .then(|| pane.local_id())
                .flatten()
        };
        leaves(self.connection.layout.as_ref()?)
            .iter()
            .find_map(under)
            .filter(|pane| *pane != id)
    }

    /// Make the resource `id` a tab of the slot showing `to`.
    fn join_slot(&mut self, id: u32, to: u32) {
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        let Some((session, mut doc)) = self.arrangement(&client) else {
            return;
        };
        if doc
            .add_tab(&ResourceId::local(id), &ResourceId::local(to))
            .is_ok()
        {
            self.write_arrangement(&client, session, &doc, "Not added as a tab");
        }
    }

    fn resource_in_session(&self, id: u32, key: SessionKey, session: u32) -> bool {
        let live = ResourceId::local(id);
        self.connection
            .resources
            .entries
            .get(&live)
            .is_some_and(|doc| doc.record.session == key)
            || self
                .connection
                .topology
                .as_ref()
                .and_then(|t| t.pane(&live))
                .is_some_and(|pane| pane.session_id == session)
    }

    /// Move a tab's group to where `to`'s is, in the session's arrangement.
    fn move_tab(&mut self, from: [u8; 16], to: [u8; 16]) {
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        let Some((session, mut doc)) = self.arrangement(&client) else {
            return;
        };
        let at = |id| doc.groups.iter().position(|g| g.id == id);
        if let (Some(from), Some(to)) = (at(from), at(to))
            && doc.move_group(from, to)
        {
            self.write_arrangement(&client, session, &doc, "Tab not moved");
        }
    }

    /// Move a session to where `to` is, writing the whole order back.
    fn move_session(&mut self, from: SessionKey, to: SessionKey) {
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        let keys = self
            .ordered_sessions()
            .iter()
            .filter_map(|s| s.key)
            .collect();
        let Some(keys) = moved(keys, from, to) else {
            return;
        };
        client.queue_frame(&FrameKind::SetMetadata {
            request_id: client.next_request_id(),
            scope: Scope::Global,
            key: SESSION_ORDER_KEY.to_owned(),
            value: session_order::encode(&keys),
        });
    }

    /// The one drain of this window's watched Global keys: the session
    /// order, watched on every client, and the agent stats the fleet views
    /// ask for. True when either changed.
    pub(super) fn take_globals(&mut self, client: &Arc<Client>) -> bool {
        let mut changed = false;
        if !Weak::ptr_eq(&self.connection.reorder.watching, &Arc::downgrade(client)) {
            // Remembered by the runtime, and asked again on every attach.
            let _ = client.watch_global_metadata(SESSION_ORDER_KEY.to_owned());
            self.connection.reorder.watching = Arc::downgrade(client);
            changed = !std::mem::take(&mut self.connection.reorder.order).is_empty();
        }
        for value in client.take_global_metadata() {
            if value.key == SESSION_ORDER_KEY {
                self.connection.reorder.order = session_order::decode(value.value.as_deref());
                changed = true;
            } else if value.key == creature::KEY {
                self.fleet.here = Stats::parse(value.value.as_deref(), self.clock);
                changed = true;
            }
        }
        changed |= self.place_sidebar_moves(client);
        changed
    }
}

#[cfg(test)]
mod tests;
