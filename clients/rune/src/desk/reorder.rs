//! Drag to reorder tabs and sessions. The order shown is always the
//! server's: a tab moves within the session's arrangement, as the terminal
//! UI's `move-window` and tab drag write it, and sessions follow the
//! [`SESSION_ORDER_KEY`] Global metadata key every window watches. A drag
//! only marks where the drop lands; nothing moves until the server says so.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use phux_client_runtime::Client;
use phux_client_runtime::control::SessionDescriptor;
use phux_client_runtime::session_order::{self, SESSION_ORDER_KEY};
use phux_protocol::ids::SessionKey;
use phux_protocol::wire::frame::{FrameKind, Scope};
use rune::{Drag, Phase};

use super::Desk;
use crate::creature::{self, Stats};

/// What a drag picked up, or is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Grab {
    Tab([u8; 16]),
    Session(SessionKey),
}

#[derive(Default)]
pub struct Reorder {
    /// The item held and the one under the pointer, while dragging.
    pub held: Option<(Grab, Grab)>,
    /// Where each tab and session handle last laid out, in window points.
    rects: HashMap<Grab, [f32; 4]>,
    /// The server's session order, as last heard.
    order: Vec<SessionKey>,
    /// The client the order key is watched on.
    watching: Weak<Client>,
}

impl Reorder {
    pub fn laid_out(&mut self, grab: Grab, rect: [f32; 4]) {
        self.rects.insert(grab, rect);
    }

    /// The server's session order, as last heard.
    pub fn order(&self) -> &[SessionKey] {
        &self.order
    }

    /// The item a drop would land on, when it is not the one held.
    pub fn target(&self) -> Option<Grab> {
        self.held
            .and_then(|(from, over)| (from != over).then_some(over))
    }
}

/// The slot under `at` along a strip whose items start at `starts`, in
/// order: before the first is the first, past the last is the last.
pub fn slot_at(starts: &[f32], at: f32) -> usize {
    starts.iter().rposition(|start| *start <= at).unwrap_or(0)
}

/// The item under `at`, from each item's laid-out start. Items come in any
/// order: project groups show sessions in another order than stored.
fn over_at(mut strip: Vec<(Grab, f32)>, at: f32) -> Option<Grab> {
    strip.sort_by(|a, b| a.1.total_cmp(&b.1));
    let starts: Vec<f32> = strip.iter().map(|(_, start)| *start).collect();
    strip.get(slot_at(&starts, at)).map(|(grab, _)| *grab)
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
        let all = self.topology.iter().flat_map(|t| &t.sessions);
        let listed =
            |s: &&SessionDescriptor| self.quick.is_some() || !super::quick::hidden(&s.name);
        let mut sessions: Vec<_> = all.filter(listed).collect();
        session_order::list(&mut sessions, &self.reorder.order, |s| s.key, |s| s.touched);
        sessions
    }

    /// One phase of a tab or session drag: mark what is under the pointer,
    /// and on release move the held item there.
    pub fn drag_reorder(&mut self, grab: Grab, drag: &Drag) {
        if drag.phase == Phase::Down {
            self.reorder.held = None;
            return;
        }
        let [x, y] = drag.window_point();
        let strip: Vec<(Grab, f32)> = match grab {
            Grab::Tab(_) => self
                .tabs
                .list
                .iter()
                .map(|tab| Grab::Tab(tab.id))
                .filter_map(|g| Some((g, self.reorder.rects.get(&g)?[0])))
                .collect(),
            Grab::Session(_) => self
                .ordered_sessions()
                .iter()
                .filter_map(|s| Some(Grab::Session(s.key?)))
                .filter_map(|g| Some((g, self.reorder.rects.get(&g)?[1])))
                .collect(),
        };
        let at = if matches!(grab, Grab::Tab(_)) { x } else { y };
        let Some(over) = over_at(strip, at) else {
            return;
        };
        if drag.phase == Phase::Move {
            self.reorder.held = Some((grab, over));
            return;
        }
        self.reorder.held = None;
        if drag.cancelled() || over == grab {
            return;
        }
        match (grab, over) {
            (Grab::Tab(from), Grab::Tab(to)) => self.move_tab(from, to),
            (Grab::Session(from), Grab::Session(to)) => self.move_session(from, to),
            _ => {}
        }
    }

    /// Move a tab's group to where `to`'s is, in the session's arrangement.
    fn move_tab(&mut self, from: [u8; 16], to: [u8; 16]) {
        let Some(client) = self.client.clone() else {
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
        let Some(client) = self.client.clone() else {
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
        if !Weak::ptr_eq(&self.reorder.watching, &Arc::downgrade(client)) {
            // Remembered by the runtime, and asked again on every attach.
            let _ = client.watch_global_metadata(SESSION_ORDER_KEY.to_owned());
            self.reorder.watching = Arc::downgrade(client);
            changed = !std::mem::take(&mut self.reorder.order).is_empty();
        }
        for value in client.take_global_metadata() {
            if value.key == SESSION_ORDER_KEY {
                self.reorder.order = session_order::decode(value.value.as_deref());
                changed = true;
            } else if value.key == creature::KEY {
                self.fleet.here = Stats::parse(value.value.as_deref(), self.clock);
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::{Grab, SessionKey, moved, over_at, slot_at};

    #[test]
    fn a_drop_lands_on_the_slot_under_the_pointer() {
        let starts = [10.0, 60.0, 140.0];
        assert_eq!(slot_at(&starts, 0.0), 0, "before the first");
        assert_eq!(slot_at(&starts, 10.0), 0);
        assert_eq!(slot_at(&starts, 59.0), 0);
        assert_eq!(slot_at(&starts, 60.0), 1);
        assert_eq!(slot_at(&starts, 139.9), 1);
        assert_eq!(slot_at(&starts, 500.0), 2, "past the last");
        assert_eq!(slot_at(&[], 5.0), 0);
    }

    /// Stored a, b, c, d with b and d grouped under a project, the sidebar
    /// shows b, d, a, c. The pointer picks what is drawn under it, and the
    /// drop moves the held session in the whole stored order.
    #[test]
    fn a_grouped_drop_moves_in_the_whole_stored_order() {
        let key = |n| SessionKey::new([n; 16]).expect("nonzero");
        let (a, b, c, d) = (key(1), key(2), key(3), key(4));
        let strip = |grabs: &[(SessionKey, f32)]| {
            grabs
                .iter()
                .map(|(k, y)| (Grab::Session(*k), *y))
                .collect::<Vec<_>>()
        };
        // In stored order, laid out grouped: b 0, d 30, a 60, c 90.
        let laid_out = strip(&[(a, 60.0), (b, 0.0), (c, 90.0), (d, 30.0)]);
        assert_eq!(over_at(laid_out.clone(), 65.0), Some(Grab::Session(a)));
        assert_eq!(over_at(laid_out.clone(), 35.0), Some(Grab::Session(d)));
        assert_eq!(over_at(laid_out, 200.0), Some(Grab::Session(c)));
        // Dropping c on a puts c where a stood in the stored order.
        assert_eq!(moved(vec![a, b, c, d], c, a), Some(vec![c, a, b, d]));
        assert_eq!(moved(vec![a, b], a, key(9)), None);
    }
}
