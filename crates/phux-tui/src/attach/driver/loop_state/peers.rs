//! Peer sessions, persisted layouts, and cross-session focus.

use super::{
    AttachError, Connection, DEFAULT_GROUP_ID, FrameKind, FrameOutcome, HashMap,
    RepaintAccumulator, ResourceId, Scope, SidebarReservation, emit_view_reflow,
    encode_layout_or_log, layout_key, reanchor_predict_to_pane, send_unless_peer_gone,
};

impl super::SessionLoop {
    /// Fold a peer layout reply, then sync that peer's agent-record watches.
    pub(super) async fn fold_peer_layout(
        &mut self,
        conn: &mut Connection,
        request_id: u32,
        value: Option<&[u8]>,
        repaint: &mut RepaintAccumulator,
    ) -> Result<(), AttachError> {
        let Some(session) = self.peers.foreign_layout_pending.remove(&request_id) else {
            return Ok(());
        };
        self.peers.apply_layout_reply(session, value);
        self.reconcile_peer_agents(conn).await?;
        self.peers.chrome_dirty = true;
        repaint.raise_fleet();
        Ok(())
    }

    /// Prune and re-sync the foreign agent watches against the live foreign
    /// terminal set (from persisted layouts, or the server graph).
    pub(super) async fn reconcile_peer_agents(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        self.peers
            .sweep_agents(conn, &mut self.next_request_id, &self.review)
            .await
    }

    /// Take over the review index an earlier entry on this
    /// connection handed out at a session switch.
    pub(in super::super) fn set_review(&mut self, review: crate::attach::review::ReviewIndex) {
        self.review = review;
    }

    /// Fold a foreign agent record into the fleet cache and review index;
    /// true when either moved.
    pub(super) fn fold_foreign_agent(&mut self, id: &ResourceId, value: Option<&[u8]>) -> bool {
        let cache_changed = self.peers.apply_agent_reply(id.clone(), value);
        let review_changed = self.review.observe_record(
            id,
            self.peers.foreign_agents.get(id),
            self.mirror.focused_resource.as_ref(),
        );
        cache_changed || review_changed
    }

    /// Fold what the frame said about other sessions into the peer caches.
    /// Raises both chrome and fleet: peer state feeds the always-on strip.
    pub(super) async fn fold_peer_outcome(
        &mut self,
        conn: &mut Connection,
        outcome: &mut FrameOutcome,
        repaint: &mut RepaintAccumulator,
    ) -> Result<(), AttachError> {
        let layout_folded = if let Some((session, value)) = outcome.foreign_layout.take() {
            self.peers.apply_layout_reply(session, value.as_deref());
            self.reconcile_peer_agents(conn).await?;
            true
        } else {
            false
        };
        let agent_folded = if let Some((id, value)) = outcome.foreign_agent.take() {
            self.fold_foreign_agent(&id, value.as_deref())
        } else {
            false
        };
        // Only a NEW ask is a repaint reason; a repeated one
        // changes nothing the strip renders.
        let asked_folded = outcome
            .foreign_attention
            .take()
            .is_some_and(|id| self.peers.foreign_attention.insert(id));
        let cleared_folded = outcome
            .foreign_attention_clear
            .take()
            .is_some_and(|id| self.peers.foreign_attention.remove(&id));
        // Lifecycle changes owe a real graph/layout sweep after this burst.
        self.peers.sweep_pending |= outcome.foreign_pane_set_dirty;
        if layout_folded
            || agent_folded
            || asked_folded
            || cleared_folded
            || outcome.foreign_pane_set_dirty
        {
            self.peers.chrome_dirty = true;
            repaint.raise_fleet();
        }
        Ok(())
    }

    /// Re-sweep the watches and caches an ATTACHED snapshot or a pane
    /// lifecycle change invalidated.
    pub(super) async fn resync_watches(
        &mut self,
        conn: &mut Connection,
        outcome: &mut FrameOutcome,
    ) -> Result<(), AttachError> {
        // Keep a `phux.agent/v1` watch per live pane, once bootstrap outbound
        // has gone (see `bootstrap_outbound`).
        if self.bootstrap_outbound.is_none()
            && self.mirror.panes.len() != self.mirror.agent_meta.subscribed.len()
        {
            self.sync_agent_meta(conn).await?;
        }
        // The ATTACHED snapshot refreshes the pane-cwd index
        // behind the sidebar branch line.
        self.vcs
            .apply_snapshot(std::mem::take(&mut outcome.pane_cwds));
        // Refresh the cached session graph and re-sweep the peers against it.
        if let Some((list, focused)) = outcome.sessions.take() {
            self.peers.sessions = list;
            self.peers.focused_session = Some(focused);
            self.fold_inventory(std::mem::take(&mut outcome.inventory));
            // This sweep satisfies a pending deferred one; clearing the flag
            // avoids a duplicate GET per peer.
            self.peers.sweep_pending = false;
            self.sweep_peer_layouts(conn).await?;
        } else {
            self.fold_inventory(std::mem::take(&mut outcome.inventory));
        }
        Ok(())
    }

    /// Broadcast the local workspace on the session's layout key so sibling
    /// clients reconcile.
    pub(super) async fn broadcast_layout(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        if !self.layout_read_complete {
            return Ok(());
        }
        let Some(session) = self.peers.focused_session else {
            return Ok(());
        };
        let Some(bytes) = encode_layout_or_log(&self.mirror.workspace) else {
            return Ok(());
        };
        let request_id = self.take_request_id();
        send_unless_peer_gone(
            conn,
            &FrameKind::SetMetadata {
                request_id,
                scope: Scope::Group(DEFAULT_GROUP_ID),
                key: layout_key(session),
                value: bytes,
            },
        )
        .await
    }

    /// ADR-0105: tombstone the stored layout once a keep-empty session's last
    /// pane closed.
    pub(super) async fn clear_stored_layout(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        let Some(session) = self.peers.focused_session else {
            return Ok(());
        };
        let request_id = self.take_request_id();
        send_unless_peer_gone(
            conn,
            &FrameKind::DeleteMetadata {
                request_id,
                scope: Scope::Group(DEFAULT_GROUP_ID),
                key: layout_key(session),
            },
        )
        .await
    }

    /// A close/spawn changed survivors' dimensions: resize each changed leaf's
    /// PTY, before the repaint so the resync snapshot lands on the grown mirror.
    pub(super) async fn emit_reflow_resizes(
        &self,
        conn: &mut Connection,
        prev_rects: &HashMap<ResourceId, crate::layout::Rect>,
        sidebar: Option<SidebarReservation>,
    ) -> Result<(), AttachError> {
        emit_view_reflow(
            conn,
            &self.mirror.workspace,
            self.mirror.zoomed.as_ref(),
            prev_rects,
            self.content(sidebar),
            self.resize_cell_px,
        )
        .await
    }

    /// The layout changed under us: raise a full repaint (deferred while an
    /// overlay is up; dismiss repaints).
    pub(super) fn on_layout_replaced(
        &mut self,
        sidebar: Option<SidebarReservation>,
        repaint: &mut RepaintAccumulator,
    ) {
        self.resolve_cross_session_pick();
        self.refresh_chrome();
        // Overlays (copy mode) must adopt the focused pane's new rect.
        self.sync_overlays(sidebar);
        if !self.overlays.is_active() {
            repaint.raise_full();
        }
    }

    /// Apply a one-step cross-session pick: a `ResourceId` from the server
    /// graph, else the layout-backed window/pane indices.
    pub(super) fn resolve_cross_session_pick(&mut self) {
        if let Some(id) = self.pick.resource.take() {
            self.pick.window = None;
            self.pick.pane = None;
            self.focus_pending_resource(&id);
            return;
        }
        let Some(idx) = self.pick.window.take() else {
            return;
        };
        if !self.mirror.workspace.select(idx) {
            tracing::warn!(
                index = idx,
                windows = self.mirror.workspace.windows.len(),
                "cross-session window pick out of range; keeping restored focus",
            );
            return;
        }
        let next_focus = self
            .mirror
            .workspace
            .active_window()
            .and_then(|ls| ls.focus.clone());
        self.focus_history
            .transition(&mut self.mirror.focused_resource, next_focus);
        if let Some(ord) = self.pick.pane.take() {
            self.focus_picked_leaf(idx, ord);
        }
        if let Some(fid) = self.mirror.focused_resource.as_ref() {
            reanchor_predict_to_pane(&mut self.mirror.predict, &self.mirror.panes, fid);
        }
    }

    /// Focus `id` in the current workspace, adopting a server-graph window
    /// when the TUI layout does not yet name it.
    pub(super) fn focus_pending_resource(&mut self, id: &ResourceId) {
        if self.focus_resource_in_workspace(id) {
            return;
        }
        if !self.adopt_inventory_resource(id) {
            tracing::warn!(
                resource = %id,
                "cross-session resource pick not in workspace or inventory; keeping restored focus",
            );
            return;
        }
        if !self.focus_resource_in_workspace(id) {
            tracing::warn!(
                resource = %id,
                "cross-session resource pick adopted but not focusable",
            );
        }
    }

    pub(super) fn focus_resource_in_workspace(&mut self, id: &ResourceId) -> bool {
        let Some(idx) = self.mirror.workspace.windows.iter().position(|window| {
            window
                .state
                .tree
                .as_ref()
                .is_some_and(|tree| crate::layout::leaves(tree).iter().any(|leaf| leaf == id))
        }) else {
            return false;
        };
        let _ = self.mirror.workspace.select(idx);
        if let Some(ls) = self.mirror.workspace.active_window_mut() {
            ls.focus = Some(id.clone());
        }
        self.focus_history
            .transition(&mut self.mirror.focused_resource, Some(id.clone()));
        reanchor_predict_to_pane(&mut self.mirror.predict, &self.mirror.panes, id);
        true
    }

    pub(super) fn adopt_inventory_resource(&mut self, id: &ResourceId) -> bool {
        let Some(window) = self.peers.windows.iter().find(|window| {
            self.peers
                .focused_session
                .is_none_or(|session| window.session_id == session)
                && crate::attach::sidebar_zones::window_contains_terminal(
                    window,
                    &self.peers.resources,
                    id,
                )
        }) else {
            return false;
        };
        // `GET_STATE` carries no layout (ADR-0030): adopt the pane alone.
        let layout = crate::layout::LayoutNode::Leaf(id.clone());
        let name = window.name.clone();
        let inventory_leaves = crate::layout::leaves(&layout);
        if self.mirror.workspace.windows.len() == 1 {
            let bootstrap = self.mirror.workspace.windows[0]
                .state
                .tree
                .as_ref()
                .map(crate::layout::leaves)
                .unwrap_or_default();
            if bootstrap.len() == 1 && inventory_leaves.iter().any(|leaf| bootstrap.contains(leaf))
            {
                let w = &mut self.mirror.workspace.windows[0];
                w.name = name;
                w.state.tree = Some(layout);
                w.state.focus = Some(id.clone());
                self.mirror.workspace.active = 0;
                return true;
            }
        }
        self.mirror.workspace.add_window(name, id.clone());
        if let Some(ls) = self.mirror.workspace.active_window_mut() {
            ls.tree = Some(layout);
            ls.focus = Some(id.clone());
        }
        true
    }

    /// Focus leaf `ord` of the just-selected window; out of range keeps the
    /// restored focus.
    pub(super) fn focus_picked_leaf(&mut self, idx: usize, ord: usize) {
        let picked = self
            .mirror
            .workspace
            .active_window()
            .and_then(|ls| ls.tree.as_ref())
            .map(crate::layout::leaves)
            .and_then(|leaves| leaves.get(ord).cloned());
        let Some(leaf) = picked else {
            tracing::warn!(
                window = idx,
                pane = ord,
                "cross-session pane pick out of range; keeping window focus",
            );
            return;
        };
        if let Some(ls) = self.mirror.workspace.active_window_mut() {
            ls.focus = Some(leaf.clone());
        }
        self.focus_history
            .transition(&mut self.mirror.focused_resource, Some(leaf));
    }
}
