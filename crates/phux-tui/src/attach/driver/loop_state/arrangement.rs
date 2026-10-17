//! The attached session's arrangement record on an `ORGANIZATION` server:
//! open the link, read and adopt the record, write local layout changes.
//!
//! A read is handed to the frame handler as the layout value the legacy
//! `phux.tui.layout/v1` key would carry, so both paths share one
//! reconciliation (focus kept, unknown leaves attached, the empty state).

use phux_protocol::ids::SessionId;

use super::{
    AttachError, Connection, DEFAULT_GROUP_ID, FrameKind, Notice, RepaintAccumulator,
    SidebarReservation, Step, encode_layout_or_log, layout_key,
};
use crate::attach::arrangement_link::ArrangementLink;
use crate::layout::ArrangementDoc;

impl super::SessionLoop {
    /// Open the arrangement link for `session` when the server advertises
    /// `ORGANIZATION` and a control dial exists. `true` when the link is up
    /// and owes its first read; `false` keeps the legacy key.
    pub(super) async fn open_arrangement_link(&mut self, session: SessionId) -> bool {
        self.arrangement = None;
        if !self.organization_supported {
            return false;
        }
        let Some(dial) = self.control_dial.as_deref() else {
            return false;
        };
        match ArrangementLink::open(dial, session).await {
            Ok(Some(link)) => {
                self.arrangement = Some(link);
                self.arrangement_read_owed = true;
                true
            }
            Ok(None) => false,
            Err(err) => {
                tracing::warn!(error = %err, "arrangement link failed; using the layout key");
                false
            }
        }
    }

    /// Read the arrangement record when a read is owed and adopt it through
    /// the frame handler: as the attach's layout reply before the first read
    /// completed, as a layout change after. `Some` ends the attach.
    pub(super) async fn settle_arrangement_read<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<Step>, AttachError> {
        if !std::mem::take(&mut self.arrangement_read_owed) {
            return Ok(None);
        }
        let Some(link) = self.arrangement.as_mut() else {
            return Ok(None);
        };
        let session = link.session();
        let read = link.read().await;
        let frame = match (read, self.layout_get_request_id) {
            // The attach's first read: the reply the layout GET would carry.
            (Ok(document), Some(request_id)) => FrameKind::MetadataValue {
                request_id,
                value: document.as_ref().and_then(encode_layout_or_log),
            },
            (Ok(Some(document)), None) => match encode_layout_or_log(&document) {
                Some(bytes) => layout_change(session, bytes),
                None => return Ok(None),
            },
            // No record yet, outside the first read: nothing to adopt.
            (Ok(None), None) => return Ok(None),
            (Err(err), first) => {
                tracing::warn!(error = %err, "arrangement read failed");
                self.apply_notices(
                    vec![Notice::warn(format!("layout not loaded: {err}"))],
                    repaint,
                );
                // The first read still has to release the write fence.
                let Some(request_id) = first else {
                    return Ok(None);
                };
                FrameKind::MetadataValue {
                    request_id,
                    value: None,
                }
            }
        };
        self.apply_frame_step(conn, out, sidebar, frame, false, repaint)
            .await
    }

    /// Read the peer arrangement records a sweep or an `ARRANGEMENT_CHANGED`
    /// asked for, fold them into the peer caches, and re-sync the peer agent
    /// watches against them. A failed read keeps the caches as they were.
    /// `true` when the caches were refreshed and the chrome owes a repaint.
    pub(super) async fn settle_peer_arrangement_reads(
        &mut self,
        conn: &mut Connection,
        repaint: &mut RepaintAccumulator,
    ) -> Result<bool, AttachError> {
        use super::super::subscriptions::PeerArrangementReads;
        let (full, only) = match std::mem::take(&mut self.peers.arrangement_reads_owed) {
            PeerArrangementReads::Settled => return Ok(false),
            PeerArrangementReads::All => (true, None),
            PeerArrangementReads::Sessions(sessions) => (false, Some(sessions)),
        };
        let Some(link) = self.arrangement.as_mut() else {
            return Ok(false);
        };
        match link.read_peers(only.as_ref()).await {
            Ok(reads) => self.peers.apply_arrangement_reads(reads, full),
            Err(err) => {
                tracing::warn!(error = %err, "peer arrangement read failed");
                return Ok(false);
            }
        }
        self.reconcile_peer_agents(conn).await?;
        self.peers.chrome_dirty = true;
        repaint.raise_fleet();
        Ok(true)
    }

    /// Write `document` as the session's arrangement record. `false` when
    /// there is no link (the caller uses the legacy key).
    pub(super) async fn write_arrangement(&mut self, document: &ArrangementDoc) -> bool {
        let Some(link) = self.arrangement.as_mut() else {
            return false;
        };
        if let Err(err) = link.write(document).await {
            tracing::warn!(error = %err, "arrangement write failed");
            self.show_notices([Notice::warn(format!("layout not saved: {err}"))]);
        }
        true
    }
}

/// The change broadcast a legacy server sends for this session's layout key.
fn layout_change(session: SessionId, bytes: Vec<u8>) -> FrameKind {
    FrameKind::MetadataChanged {
        scope: phux_protocol::wire::frame::Scope::Group(DEFAULT_GROUP_ID),
        key: layout_key(session),
        value: Some(bytes),
        actor: None,
    }
}
