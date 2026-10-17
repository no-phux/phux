//! The session-switch hand-over: what one loop entry passes the next so a
//! switch swaps the glass in one frame instead of blanking it.
//!
//! Two halves. The paint hold keeps the previous session's frame on screen
//! (no unsynchronized clear, no cursor jump) until the new session's layout
//! and visible pane bootstraps have landed, then paints the new view in one
//! DEC 2026 frame; a deadline bounds the hold on a slow link. The carried
//! peer caches seed the new entry's sidebar with what the server-wide sweeps
//! already knew (machine label, peer layouts, every agent record), so the
//! chrome does not paint empty and refill as the re-sweep answers.

use std::collections::{HashMap, HashSet};

use phux_client::agent_meta::AgentRecord;
use phux_protocol::ids::{ResourceId, SessionId};

use super::{RepaintLevel, SidebarReservation, view_rects};
use crate::layout::ArrangementDoc;

/// Longest a switch keeps the previous frame up waiting for the new
/// session's view. Past it the view paints with whatever has landed and the
/// rest fills in place, as a first attach does.
pub(super) const SWITCH_PAINT_HOLD: std::time::Duration = std::time::Duration::from_millis(350);

/// Server-wide peer caches carried across an in-process session switch.
///
/// Everything here is re-read by the new entry's sweeps; the carry only
/// spares the first frame from painting without it.
#[derive(Debug, Default)]
pub(in super::super) struct CarriedPeers {
    /// The serving machine's label from the whoami read.
    serving_host: Option<String>,
    /// Peer sessions' arrangements, plus the session being left, whose
    /// layout this client held locally.
    layouts: HashMap<SessionId, ArrangementDoc>,
    /// Every agent record this client knew: the old session's own panes and
    /// the foreign ones.
    agents: HashMap<ResourceId, AgentRecord>,
    /// Foreign panes whose agent asked for a human.
    attention: HashSet<ResourceId>,
    /// Session project tags.
    project_tags: HashMap<SessionId, String>,
    /// The federation host inventory.
    hosts: Vec<phux_protocol::wire::info::HostInventory>,
}

impl super::SessionLoop {
    /// Keep the previous session's frame on the glass until this entry's
    /// view is ready (see the module docs).
    pub(in super::super) fn hold_paint_for_switch(&mut self) {
        self.paint_hold = Some(tokio::time::Instant::now() + SWITCH_PAINT_HOLD);
    }

    /// Whether a switch is still holding the previous frame.
    pub(super) const fn paint_held(&self) -> bool {
        self.paint_hold.is_some()
    }

    /// The hold's deadline, for the `select!` timer arm.
    pub(super) const fn paint_hold_deadline(&self) -> Option<tokio::time::Instant> {
        self.paint_hold
    }

    /// The new view can paint whole: the layout read settled (or none was
    /// sent), no attach barrier is held, and every pane the view tiles has a
    /// published grid.
    fn switch_view_ready(&self, sidebar: Option<SidebarReservation>) -> bool {
        let layout_settled = self.layout_read_complete
            || (self.bootstrap_outbound.is_none() && self.layout_get_request_id.is_none());
        if !layout_settled || self.pending_attach_ready.is_some() {
            return false;
        }
        view_rects(
            &self.mirror.workspace,
            self.mirror.zoomed.as_ref(),
            self.content(sidebar),
            self.viewport_dims,
        )
        .keys()
        .all(|id| {
            self.mirror.panes.contains_key(id)
                && crate::attach::pane_state::published_terminal(&self.mirror.engine_kernel, id)
                    .is_some()
        })
    }

    /// End the hold once the view is ready, an overlay wants the screen, or
    /// the deadline passed, and paint the new session over the old frame in
    /// one clear-and-recompose.
    pub(super) fn release_paint_hold_if_ready<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        let Some(deadline) = self.paint_hold else {
            return;
        };
        let expired = tokio::time::Instant::now() >= deadline;
        if !expired && !self.overlays.is_active() && !self.switch_view_ready(sidebar) {
            return;
        }
        self.paint_hold = None;
        // The full frame below redraws every pane the hold deferred.
        self.pacer.clear_pending();
        if self.overlays.is_active() {
            self.paint_overlay(out, sidebar);
        } else {
            self.repaint_view(out, sidebar, RepaintLevel::Full);
        }
    }

    /// The peer caches the next entry seeds from, taken as this loop exits
    /// for a switch.
    pub(super) fn peers_for_switch(&mut self) -> CarriedPeers {
        let mut agents = std::mem::take(&mut self.peers.foreign_agents);
        agents.extend(self.mirror.agent_meta.records.drain());
        let mut layouts = std::mem::take(&mut self.peers.foreign_layouts);
        if let Some(session) = self.peers.focused_session
            && !self.mirror.workspace.groups.is_empty()
        {
            layouts.insert(session, self.mirror.workspace.clone());
        }
        CarriedPeers {
            serving_host: self.peers.serving_host.take(),
            layouts,
            agents,
            attention: std::mem::take(&mut self.peers.foreign_attention),
            project_tags: std::mem::take(&mut self.peers.project_tags),
            hosts: std::mem::take(&mut self.peers.hosts),
        }
    }

    /// Seed this entry's peer caches from a switch's carry. Subscriptions
    /// are not carried: `DETACH` dropped them, and the sweeps re-open them.
    pub(in super::super) fn seed_carried_peers(&mut self, carried: CarriedPeers) {
        if carried.serving_host.is_some() {
            self.peers.serving_host = carried.serving_host;
            self.peers.serving_host_attempted = true;
        }
        self.peers.foreign_layouts = carried.layouts;
        self.peers.foreign_agents = carried.agents;
        self.peers.foreign_attention = carried.attention;
        self.peers.project_tags = carried.project_tags;
        self.peers.hosts = carried.hosts;
    }

    /// Once `ATTACHED` named this session, move its own panes' carried agent
    /// records from the foreign cache into the local index, and drop the
    /// session's own entry from the peer layouts.
    pub(super) fn adopt_carried_local_agents(&mut self) {
        if let Some(session) = self.peers.focused_session {
            self.peers.foreign_layouts.remove(&session);
        }
        let local: Vec<ResourceId> = self
            .peers
            .foreign_agents
            .keys()
            .filter(|id| self.mirror.panes.contains_key(*id))
            .cloned()
            .collect();
        for id in local {
            if let Some(record) = self.peers.foreign_agents.remove(&id) {
                self.mirror.agent_meta.records.insert(id, record);
            }
        }
    }
}
