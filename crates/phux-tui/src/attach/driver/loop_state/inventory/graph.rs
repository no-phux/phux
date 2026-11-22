//! The cached session graph: windows, resources, members, and the
//! placeholder leaves the resources name.

impl super::super::SessionLoop {
    /// Cache windows/resources from a snapshot when it actually carries them.
    /// `true` when it reclassified or renamed an arranged leaf, whose tile
    /// must repaint.
    pub(in crate::attach::driver::loop_state) fn adopt_snapshot_graph(
        &mut self,
        snapshot: &phux_protocol::wire::info::SessionSnapshot,
    ) -> bool {
        if !snapshot.windows.is_empty() {
            self.peers.windows.clone_from(&snapshot.windows);
        }
        let mut retiled = false;
        if !snapshot.resources.is_empty() {
            self.peers.resources.clone_from(&snapshot.resources);
            let changed = self.mirror.leaves.learn(&snapshot.resources);
            retiled = changed.iter().any(|id| self.mirror.arranges(id));
            self.refresh_members();
        }
        if let Some(info) = snapshot
            .sessions
            .iter()
            .find(|session| Some(session.id) == self.peers.focused_session)
        {
            self.mirror.lifetime = crate::attach::session_mirror::session_lifetime(info);
        }
        retiled
    }

    /// Recompute the attached session's members from the cached graph.
    pub(in crate::attach::driver::loop_state) fn refresh_members(&mut self) {
        let Some(session) = self.peers.focused_session else {
            return;
        };
        self.mirror.members = crate::attach::sidebar_zones::session_terminal_ids(
            &self.peers.resources,
            &self.peers.windows,
            session,
        )
        .into_iter()
        .collect();
    }

    /// Fold windows/resources carried on an ATTACHED outcome.
    pub(in crate::attach::driver::loop_state) fn fold_inventory(
        &mut self,
        inventory: Option<(
            Vec<phux_protocol::wire::info::WindowInfo>,
            Vec<phux_protocol::wire::info::ResourceInfo>,
        )>,
    ) {
        let Some((windows, resources)) = inventory else {
            return;
        };
        if !windows.is_empty() {
            self.peers.windows = windows;
        }
        if !resources.is_empty() {
            let _ = self.mirror.leaves.learn(&resources);
            self.peers.resources = resources;
        }
    }
}
