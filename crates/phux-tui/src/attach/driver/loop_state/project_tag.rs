//! The one `phux.session.project/v1` tag, and the global metadata reads that
//! travel with the sidebar sweep.

use super::{
    AttachError, Connection, FrameKind, FrameOutcome, RepaintAccumulator, Scope, apply_graph_rename,
};

impl super::SessionLoop {
    /// Global metadata the picker and status line follow after attach.
    pub(super) async fn subscribe_global_session_keys(
        &self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        use phux_protocol::wire::frame::{
            CONFIG_RELOAD_KEY, SESSION_KEEP_EMPTY_KEY, SESSION_NAME_KEY, SESSION_PROJECT_KEY,
        };
        for key in [
            CONFIG_RELOAD_KEY,
            SESSION_KEEP_EMPTY_KEY,
            SESSION_NAME_KEY,
            SESSION_PROJECT_KEY,
        ] {
            conn.send(&FrameKind::SubscribeMetadata {
                scope: Scope::Global,
                key: key.to_owned(),
            })
            .await?;
        }
        Ok(())
    }

    /// Serving host, the stored project tag, then the host inventory.
    pub(super) async fn request_sidebar_facts(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        self.request_serving_host(conn).await?;
        self.request_project_tag(conn).await?;
        self.request_host_inventory(conn).await
    }

    /// Read the one stored project tag. The subscribe only delivers later writes.
    pub(super) async fn request_project_tag(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        if self.peers.project_tag_pending.is_some() {
            return Ok(());
        }
        let request_id = self.take_request_id();
        self.peers.project_tag_pending = Some(request_id);
        super::super::session_io::send_unless_peer_gone(
            conn,
            &FrameKind::GetMetadata {
                request_id,
                scope: Scope::Global,
                key: phux_protocol::wire::frame::SESSION_PROJECT_KEY.to_owned(),
            },
        )
        .await
    }

    /// Serving-host and project-tag replies. `None` means this frame was one.
    pub(super) fn take_sidebar_metadata(&mut self, frame: FrameKind) -> Option<FrameKind> {
        let frame = self.take_serving_host_reply(frame)?;
        self.take_project_tag_reply(frame)
    }

    fn take_serving_host_reply(&mut self, frame: FrameKind) -> Option<FrameKind> {
        match frame {
            FrameKind::MetadataValue { request_id, value }
                if self.peers.serving_host_pending == Some(request_id) =>
            {
                self.fold_serving_host(value.as_deref());
                None
            }
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } if self.peers.serving_host_pending == Some(request_id) => {
                self.peers.serving_host_pending = None;
                None
            }
            other => Some(other),
        }
    }

    fn take_project_tag_reply(&mut self, frame: FrameKind) -> Option<FrameKind> {
        match frame {
            FrameKind::MetadataValue { request_id, value }
                if self.peers.project_tag_pending == Some(request_id) =>
            {
                self.fold_project_tag(value.as_deref());
                None
            }
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } if self.peers.project_tag_pending == Some(request_id) => {
                self.peers.project_tag_pending = None;
                None
            }
            other => Some(other),
        }
    }

    /// Join the stored project tag to the session that still has that name.
    pub(super) fn fold_project_tag(&mut self, value: Option<&[u8]>) {
        self.peers.project_tag_pending = None;
        self.peers.stored_project_tag = value
            .and_then(phux_protocol::wire::frame::decode_session_project)
            .map(|(name, project)| (name.to_owned(), project.to_owned()));
        self.apply_stored_project_tag();
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
        self.peers.chrome_dirty = true;
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
            self.peers.chrome_dirty = true;
            self.session_picker_dirty = true;
            self.note_chrome_change(repaint);
        }
        self.fold_project_tag_outcome(outcome);
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
