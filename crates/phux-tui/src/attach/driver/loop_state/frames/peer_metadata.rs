//! Peer metadata correlation belongs to the watch owner; the driver applies
//! the resulting facts to review, presentation and follow-up I/O.

use super::super::{AttachError, Connection, FrameKind, RepaintAccumulator};
use crate::attach::driver::subscriptions::{PeerRead, PeerReply, SidebarFact};

impl super::super::SessionLoop {
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

    pub(in crate::attach::driver::loop_state) async fn intercept_sidebar_metadata(
        &mut self,
        conn: &mut Connection,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<FrameKind>, AttachError> {
        let Some(frame) = self.take_notify_mode(frame) else {
            return Ok(None);
        };
        let (read, value) = match self.peers.take_metadata_reply(frame) {
            Ok(PeerReply::Value(read, value)) => (read, value),
            Ok(PeerReply::Refused) => return Ok(None),
            Err(frame) => return Ok(Some(frame)),
        };
        let changed = match read {
            PeerRead::Fact(SidebarFact::ServingHost) => {
                self.fold_serving_host(value.as_deref());
                false
            }
            PeerRead::Fact(SidebarFact::ProjectTag) => {
                self.fold_project_tag(value.as_deref());
                false
            }
            PeerRead::Fact(SidebarFact::SessionOrder) => {
                self.fold_session_order(phux_client::session_order::decode(value.as_deref()));
                false
            }
            PeerRead::Layout(session) => {
                self.peers.apply_layout_reply(session, value.as_deref());
                self.reconcile_peer_agents(conn).await?;
                true
            }
            PeerRead::Agent(id) => self.fold_foreign_agent(&id, value.as_deref()),
            PeerRead::Asked(id) => {
                if value.as_deref() == Some(b"1") {
                    self.peers.foreign_attention.insert(id)
                } else {
                    self.peers.foreign_attention.remove(&id)
                }
            }
        };
        if changed {
            self.peer_presentation.chrome_dirty = true;
            repaint.raise_fleet();
        }
        Ok(None)
    }
}
