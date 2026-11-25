//! The one `phux.session.project/v1` tag, the stored session order, and the
//! global metadata reads that travel with the sidebar sweep.

use super::{AttachError, Connection, FrameOutcome, RepaintAccumulator, apply_graph_rename};

impl super::SessionLoop {
    /// Serving host, the stored project tag, then the host inventory.
    pub(super) async fn request_sidebar_facts(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        self.request_serving_host(conn).await?;
        self.request_project_tag(conn).await?;
        self.request_session_order(conn).await?;
        self.request_host_inventory(conn).await
    }

    /// Read the one stored project tag. The subscribe only delivers later writes.
    pub(super) async fn request_project_tag(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        self.peers
            .request_fact(
                conn,
                &mut self.next_request_id,
                super::super::subscriptions::SidebarFact::ProjectTag,
            )
            .await
    }

    /// Read the stored session order. The subscribe only delivers later writes.
    async fn request_session_order(&mut self, conn: &mut Connection) -> Result<(), AttachError> {
        self.peers
            .request_fact(
                conn,
                &mut self.next_request_id,
                super::super::subscriptions::SidebarFact::SessionOrder,
            )
            .await
    }

    /// Adopt the stored session order; every session list redraws.
    pub(super) fn fold_session_order(&mut self, order: Vec<phux_protocol::ids::SessionKey>) {
        if order == self.peer_presentation.session_order {
            return;
        }
        self.peer_presentation.session_order = order;
        self.peer_presentation.chrome_dirty = true;
        self.session_picker_dirty = true;
    }

    /// Join the stored project tag to the session that still has that name.
    pub(super) fn fold_project_tag(&mut self, value: Option<&[u8]>) {
        self.peers.stored_project_tag = value
            .and_then(phux_protocol::wire::frame::decode_session_project)
            .map(|(name, project)| (name.to_owned(), project.to_owned()));
        self.apply_stored_project_tag();
    }

    /// Flip one project group's sidebar collapse (ADR-0168), keep it on
    /// disk, and redraw.
    pub(super) fn toggle_project_group(&mut self, key: String) {
        let collapsed = &mut self.peer_presentation.collapsed_projects;
        if !collapsed.remove(&key) {
            collapsed.insert(key);
        }
        crate::attach::client_state::save_collapsed_projects(collapsed);
        self.peer_presentation.chrome_dirty = true;
    }

    /// Apply an `agents-filter` to the sidebar's Agents panel and save it for
    /// the next attach; true when the panel's filter moved.
    pub(super) fn apply_agents_filter(
        &mut self,
        request: &crate::render::chrome::sidebar::FilterRequest,
    ) -> bool {
        let hosts = self.sidebar_painter.agent_hosts();
        let filter = request.apply(self.sidebar_painter.agent_filter(), &hosts);
        if !self.sidebar_painter.set_agent_filter(filter) {
            return false;
        }
        crate::attach::client_state::save_agent_filter(self.sidebar_painter.agent_filter());
        true
    }

    /// Replace the session list and rejoin the stored project tag.
    pub(super) fn adopt_listed_sessions(
        &mut self,
        sessions: &[phux_protocol::wire::info::SessionInfo],
    ) {
        self.peers.sessions = sessions.to_vec();
        self.apply_stored_project_tag();
    }

    /// Rebuild `project_tags` from the stored tag and the current session names.
    pub(super) fn apply_stored_project_tag(&mut self) {
        let mut next = std::collections::HashMap::new();
        if let Some((name, project)) = &self.peers.stored_project_tag
            && let Some(session) = self
                .peers
                .sessions
                .iter()
                .find(|session| session.name == *name)
        {
            next.insert(session.id, project.clone());
        }
        if next == self.peers.project_tags {
            return;
        }
        self.peers.project_tags = next;
        self.peer_presentation.chrome_dirty = true;
        self.session_picker_dirty = true;
    }

    /// Apply a session rename, then a project-tag broadcast on the same frame.
    pub(super) fn fold_session_rename(
        &mut self,
        outcome: &mut FrameOutcome,
        repaint: &mut RepaintAccumulator,
    ) {
        if let Some((current, new_name)) = outcome.session_rename.take() {
            apply_graph_rename(&mut self.peers.sessions, &current, &new_name);
            self.peer_presentation.chrome_dirty = true;
            self.session_picker_dirty = true;
            self.note_chrome_change(repaint);
        }
        if let Some((key, new_name)) = outcome.session_renamed.take() {
            if let Some(session) = self
                .peers
                .sessions
                .iter_mut()
                .find(|session| session.key == Some(key))
            {
                session.name = new_name;
            }
            self.peer_presentation.chrome_dirty = true;
            self.session_picker_dirty = true;
            self.note_chrome_change(repaint);
        }
        if let Some((key, project)) = outcome.session_project_set.take() {
            if let Some(session) = self
                .peers
                .sessions
                .iter_mut()
                .find(|session| session.key == Some(key))
            {
                phux_client::project_identity::set_link_attr(
                    &mut session.attrs,
                    project.as_deref(),
                );
            }
            self.peer_presentation.chrome_dirty = true;
            self.session_picker_dirty = true;
            self.note_chrome_change(repaint);
        }
        self.fold_project_tag_outcome(outcome);
        if let Some(order) = outcome.session_order.take() {
            self.fold_session_order(order);
            self.note_chrome_change(repaint);
        }
    }

    fn fold_project_tag_outcome(&mut self, outcome: &mut FrameOutcome) {
        use super::ProjectTagFrame;
        let stored = match std::mem::take(&mut outcome.project_tag) {
            ProjectTagFrame::Absent => return,
            ProjectTagFrame::Cleared => None,
            ProjectTagFrame::Set { name, project } => Some((name, project)),
        };
        self.peers.stored_project_tag = stored;
        self.apply_stored_project_tag();
    }
}
