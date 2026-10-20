//! Peer and sidebar metadata reply correlation.

use super::super::{AttachError, Connection, FrameKind, RepaintAccumulator};

impl super::super::SessionLoop {
    /// Fold a peer-scoped reply into the foreign caches (`None`), or hand the
    /// frame on to the general handler.
    pub(in crate::attach::driver::loop_state) async fn intercept_peer_reply(
        &mut self,
        conn: &mut Connection,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<FrameKind>, AttachError> {
        let Some(frame) = self
            .intercept_sidebar_metadata(conn, frame, repaint)
            .await?
        else {
            return Ok(None);
        };
        self.intercept_inventory_reply(conn, frame, repaint).await
    }

    /// Correlate the sidebar's metadata reads independently of command replies.
    pub(in crate::attach::driver::loop_state) async fn intercept_sidebar_metadata(
        &mut self,
        conn: &mut Connection,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<FrameKind>, AttachError> {
        let Some(frame) = self.take_serving_host_metadata(frame) else {
            return Ok(None);
        };
        let Some(frame) = self
            .take_foreign_layout_metadata(conn, frame, repaint)
            .await?
        else {
            return Ok(None);
        };
        Ok(self.take_foreign_pane_metadata(frame, repaint))
    }

    /// Sidebar metadata reads. `None` means this frame was one of them.
    fn take_serving_host_metadata(&mut self, frame: FrameKind) -> Option<FrameKind> {
        self.take_sidebar_metadata(frame)
    }

    /// A peer session's persisted-layout GET. `None` means this frame was it.
    async fn take_foreign_layout_metadata(
        &mut self,
        conn: &mut Connection,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<FrameKind>, AttachError> {
        match frame {
            FrameKind::MetadataValue { request_id, value }
                if self.peers.foreign_layout_pending.contains_key(&request_id) =>
            {
                self.fold_peer_layout(conn, request_id, value.as_deref(), repaint)
                    .await?;
                Ok(None)
            }
            other => Ok(Some(other)),
        }
    }

    /// Foreign agent-record and attention reads, plus a refused peer read.
    fn take_foreign_pane_metadata(
        &mut self,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Option<FrameKind> {
        match frame {
            // A foreign pane's agent-record GET reply.
            FrameKind::MetadataValue { request_id, value }
                if self.peers.foreign_agent_pending.contains_key(&request_id) =>
            {
                if let Some(id) = self.peers.foreign_agent_pending.remove(&request_id)
                    && self.fold_foreign_agent(&id, value.as_deref())
                {
                    self.peers.chrome_dirty = true;
                    repaint.raise_fleet();
                }
                None
            }
            FrameKind::MetadataValue { request_id, value }
                if self.peers.foreign_asked_pending.contains_key(&request_id) =>
            {
                self.fold_foreign_asked(request_id, value.as_deref(), repaint);
                None
            }
            // A refused peer read (`proto.md` §9): drop the pending entry so
            // the map does not grow and the ERROR does not reach the handler.
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } if self.peers.foreign_layout_pending.contains_key(&request_id)
                || self.peers.foreign_agent_pending.contains_key(&request_id)
                || self.peers.foreign_asked_pending.contains_key(&request_id) =>
            {
                self.peers.foreign_layout_pending.remove(&request_id);
                self.peers.foreign_agent_pending.remove(&request_id);
                self.peers.foreign_asked_pending.remove(&request_id);
                None
            }
            other => Some(other),
        }
    }

    /// Fold one foreign pane's "asked" bit and raise the fleet when it moved.
    fn fold_foreign_asked(
        &mut self,
        request_id: u32,
        value: Option<&[u8]>,
        repaint: &mut RepaintAccumulator,
    ) {
        let Some(id) = self.peers.foreign_asked_pending.remove(&request_id) else {
            return;
        };
        let asked = value == Some(b"1");
        let changed = if asked {
            self.peers.foreign_attention.insert(id)
        } else {
            self.peers.foreign_attention.remove(&id)
        };
        if changed {
            self.peers.chrome_dirty = true;
            repaint.raise_fleet();
        }
    }
}
