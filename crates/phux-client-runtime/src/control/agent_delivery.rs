//! Authoritative delivery of committed `AgentSession` publications.
//!
//! The kernel commits; this module decides when a consumer may see that
//! commit. One [`Arc`] owns the record vector. An ordinary drain accepts by
//! dropping the journal hold: that is delivery acceptance. A leased drain
//! hands out the same allocation and keeps it until
//! [`ControlPlane::accept_agent_publication`], the adapter's explicit
//! acceptance point (`phux_client_effect_clear` for C). Ending a pointer
//! borrow is not acceptance. A transport replacement returns an unaccepted
//! allocation to the replay queue. Non-agent events never enter here.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use phux_client_core::session::AgentStreamInfo;
use phux_client_core::session::agent_stream::AgentPublication;
use phux_protocol::ResourceId;

use super::{AgentSessionInfo, ControlPlane, Event};

/// Whether an unaccepted publication is waiting to be handed out.
///
/// Queued events and leased drains are the same state: already released,
/// not yet accepted. Only [`Place::Held`] is eligible for the next release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// Journaled, not yet in the event queue or a leased drain.
    Held,
    /// In the event queue or already handed to a leased drain.
    Released,
}

#[derive(Debug)]
struct Entry {
    publication: Arc<AgentPublication>,
    session: AgentSessionInfo,
    place: Place,
}

/// Undrained committed publications and the stamps a close must carry.
#[derive(Debug, Default)]
pub(super) struct AgentDelivery {
    journal: VecDeque<Entry>,
    /// Last committed stream, captured when the publication is journaled,
    /// before any detach or deletion.
    stamps: HashMap<ResourceId, AgentStreamInfo>,
    /// Closed or detached on this incarnation. A later commit for the id is
    /// dropped so a close cannot be undone by a stale effect. A new
    /// `AgentSessionDeclared` clears it.
    retired: HashSet<ResourceId>,
    /// Ids the current connection has admitted for delivery. Cleared on
    /// transport replacement; a partial `ATTACHED` does not rebuild it.
    admitted: HashSet<ResourceId>,
    /// Catalog has been applied and the attach barrier (or a browse
    /// inventory) has released on this connection.
    ready: bool,
}

impl AgentDelivery {
    /// Journal one committed publication. Returns the event to enqueue now
    /// when this connection is already ready.
    pub(super) fn commit(
        &mut self,
        publication: AgentPublication,
        session: AgentSessionInfo,
    ) -> Option<Event> {
        if self.retired.contains(&publication.resource_id) {
            return None;
        }
        let publication = Arc::new(publication);
        self.stamps
            .insert(publication.resource_id.clone(), publication.stream);
        let admitted = self.admitted.contains(&publication.resource_id);
        let place = if self.ready && admitted {
            Place::Released
        } else {
            Place::Held
        };
        let event = (place == Place::Released).then(|| Event::AgentPublication {
            publication: Arc::clone(&publication),
            session: session.clone(),
        });
        self.journal.push_back(Entry {
            publication,
            session,
            place,
        });
        event
    }

    /// Attach or browse readiness: release held publications in commit order.
    ///
    /// `session_of` borrows subscription state disjoint from this journal. It
    /// is called only for held ids, so the subscription map is not cloned.
    pub(super) fn release(
        &mut self,
        session_of: impl Fn(&ResourceId) -> AgentSessionInfo,
    ) -> Vec<Event> {
        self.ready = true;
        let mut events = Vec::new();
        for entry in &mut self.journal {
            if entry.place != Place::Held || !self.admitted.contains(&entry.publication.resource_id)
            {
                continue;
            }
            entry.session = session_of(&entry.publication.resource_id);
            entry.place = Place::Released;
            events.push(Event::AgentPublication {
                publication: Arc::clone(&entry.publication),
                session: entry.session.clone(),
            });
        }
        events
    }

    /// Transport loss or replacement. Unaccepted publications wait for the
    /// next catalog/attach readiness. Already-accepted allocations are gone.
    pub(super) fn park(&mut self) {
        self.ready = false;
        self.admitted.clear();
        for entry in &mut self.journal {
            entry.place = Place::Held;
        }
    }

    /// Committed ids this authoritative catalog no longer lists.
    /// Accepted delivery still owns a kernel cursor that must be forgotten.
    pub(super) fn ids_absent(&self, listed: &HashSet<&ResourceId>) -> Vec<ResourceId> {
        self.stamps
            .keys()
            .filter(|id| !listed.contains(*id))
            .cloned()
            .collect()
    }

    /// Ordinary drain: the consumer owns what it took.
    pub(super) fn accept_drained(&mut self, events: &[Event]) {
        for event in events {
            if let Event::AgentPublication { publication, .. } = event {
                self.accept(publication);
            }
        }
    }

    /// Drop the journal hold for this allocation. The caller removes any
    /// queued replay of the same pointer.
    pub(super) fn accept(&mut self, publication: &Arc<AgentPublication>) {
        if self
            .journal
            .front()
            .is_some_and(|entry| Arc::ptr_eq(&entry.publication, publication))
        {
            self.journal.pop_front();
            return;
        }
        self.journal
            .retain(|entry| !Arc::ptr_eq(&entry.publication, publication));
    }

    /// End one resource. The stamp is the committed watermark, taken before
    /// the kernel forgets the stream. Pending rows are discarded, not replayed.
    pub(super) fn retire(&mut self, id: &ResourceId) -> Option<AgentStreamInfo> {
        self.retired.insert(id.clone());
        self.admitted.remove(id);
        self.journal
            .retain(|entry| &entry.publication.resource_id != id);
        self.stamps.remove(id)
    }

    /// A later declaration in this incarnation may commit again.
    pub(super) fn admit(&mut self, id: &ResourceId) {
        self.retired.remove(id);
        self.admitted.insert(id.clone());
    }

    /// A new server incarnation. Ids may now name anything.
    pub(super) fn discard_incarnation(&mut self) {
        self.journal.clear();
        self.stamps.clear();
        self.retired.clear();
        self.admitted.clear();
        self.ready = false;
    }
}

pub(super) const fn is_agent_publication(event: &Event) -> bool {
    matches!(event, Event::AgentPublication { .. })
}

pub(super) fn same_publication(event: &Event, publication: &Arc<AgentPublication>) -> bool {
    matches!(
        event,
        Event::AgentPublication { publication: queued, .. } if Arc::ptr_eq(queued, publication)
    )
}

impl ControlPlane {
    pub(super) fn park_agent_delivery(&mut self) {
        self.agent_delivery.park();
        self.events.retain(|event| !is_agent_publication(event));
    }

    pub(super) fn discard_agent_delivery(&mut self) {
        self.agent_delivery.discard_incarnation();
        self.events.retain(|event| !is_agent_publication(event));
    }

    /// Release held publications once this connection is attached.
    ///
    /// No-op until catalog/attach readiness. Backlog is enqueued before any
    /// later commit in this turn. Subscription lookup borrows only that map,
    /// not the journal.
    pub(super) fn release_agent_delivery(&mut self) {
        if self.status != super::Status::Attached {
            return;
        }
        let events = {
            let Self {
                agent_delivery,
                agent_subscriptions,
                ..
            } = self;
            agent_delivery.release(|id| {
                agent_subscriptions
                    .get(id)
                    .map(|subscription| subscription.info.clone())
                    .unwrap_or_default()
            })
        };
        for event in events {
            self.push_event(event);
        }
    }

    /// Accept delivery of one leased publication by [`Arc`] allocation identity.
    ///
    /// This is consumer delivery acceptance, not the end of a pointer borrow.
    /// The C adapter invalidates effect pointers on the next mutable call,
    /// but staged effects persist until `phux_client_effect_clear`, which
    /// calls this. A generic leased drain retains the allocation until the
    /// same call. Drops the journal hold and any not-yet-drained replay of
    /// that allocation. Unknown or already-accepted pointers are ignored.
    pub fn accept_agent_publication(&mut self, publication: &Arc<AgentPublication>) {
        self.agent_delivery.accept(publication);
        self.events
            .retain(|event| !same_publication(event, publication));
    }
}
