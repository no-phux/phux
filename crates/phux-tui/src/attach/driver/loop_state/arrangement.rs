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
    /// A failed control read is not an authoritative absent layout.
    pub(super) fn arrangement_unloaded(&self) -> bool {
        self.arrangement.as_ref().is_some_and(|link| !link.loaded())
    }

    /// Open the arrangement link for `session` when the server advertises
    /// `ORGANIZATION` and a control dial exists. `true` when the link is set
    /// and owes its first read; `false` keeps the legacy key. The link
    /// connects on that read, so a control connection that cannot open is a
    /// failed read (reported, retried on the next one), never the legacy key.
    pub(super) fn open_arrangement_link(&mut self, session: SessionId) -> bool {
        self.arrangement = None;
        self.peer_reader = None;
        if !self.organization_supported {
            return false;
        }
        let Some(dial) = self.control_dial.as_deref() else {
            return false;
        };
        self.arrangement = Some(ArrangementLink::lazy(dial.clone(), session));
        // Peers' records are read off the loop on their own connection.
        self.peer_reader = Some(crate::attach::peer_arrangements::PeerReader::spawn(
            dial.clone(),
            session,
        ));
        self.arrangement_read_owed = true;
        true
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
        // The first successful read after a failure must settle held edits
        // as an initial read, including healing an authoritatively absent
        // layout. A failed read still releases only the bootstrap fence.
        if read.is_ok()
            && self.layout_get_request_id.is_none()
            && !self.deferred_arrangement_edits.is_empty()
        {
            self.layout_get_request_id = Some(self.take_request_id());
        }
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
                tracing::warn!(%session, error = %err, reason = %err, "arrangement read failed");
                self.apply_notices(
                    vec![Notice::warn(format!("layout not loaded: {err}"))],
                    repaint,
                );
                // The first read still has to release the input fence; the
                // link stays unloaded, so the frame handler neither heals
                // nor writes, and nothing is saved until a read succeeds.
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

    /// A hidden-view action can owe a retry on an otherwise quiet wire.
    /// Settle it after uncorking rather than waiting for an unrelated frame.
    pub(super) async fn settle_input_arrangement<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) -> Result<Step, AttachError> {
        let mut repaint = RepaintAccumulator::default();
        if let Some(exit) = self
            .settle_arrangement_read(conn, out, sidebar, &mut repaint)
            .await?
        {
            return Ok(exit);
        }
        self.drain_repaint(out, sidebar, &mut repaint);
        Ok(Step::Continue)
    }

    /// Hand the peer arrangement reads a sweep or an `ARRANGEMENT_CHANGED`
    /// owes to the peer reader, which retries a failed read itself; nothing
    /// to do without a reader.
    pub(super) fn request_peer_arrangement_reads(&mut self) {
        let Some(reader) = self.peer_reader.as_ref() else {
            return;
        };
        reader.request(std::mem::take(&mut self.peers.arrangement_reads_owed));
    }

    /// A peer read finished: fold it into the peer caches and re-sync the
    /// peer agent watches, then repaint what they feed.
    pub(super) async fn on_peer_reads_done<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        done: crate::attach::peer_arrangements::PeerReadDone,
    ) -> Result<Step, AttachError> {
        use crate::attach::peer_arrangements::PeerReads;
        self.peers
            .apply_arrangement_reads(done.reads, done.asked == PeerReads::All);
        self.reconcile_peer_agents(conn).await?;
        self.peers.chrome_dirty = true;
        let mut repaint = RepaintAccumulator::default();
        repaint.raise_fleet();
        self.drain_repaint(out, sidebar, &mut repaint);
        Ok(Step::Continue)
    }

    /// Write `document` as the session's arrangement record. `false` when
    /// there is no link (the caller uses the legacy key). Until a read of
    /// the record has succeeded nothing is written, since a compare-and-swap
    /// against a record this client never saw would replace it whole: the
    /// edit is reported unsaved and the read is owed again.
    pub(super) async fn write_arrangement(&mut self, document: &ArrangementDoc) -> bool {
        let Some(link) = self.arrangement.as_mut() else {
            return false;
        };
        let session = link.session();
        if !link.loaded() {
            tracing::info!(
                %session,
                groups = document.groups.len(),
                reason = "layout not loaded",
                "arrangement save refused"
            );
            self.arrangement_read_owed = true;
            self.show_notices([Notice::warn(
                "layout not saved: the stored layout is not loaded yet".to_owned(),
            )]);
            return true;
        }
        if let Err(err) = link.write(document).await {
            tracing::info!(
                %session,
                groups = document.groups.len(),
                reason = %err,
                "arrangement save refused"
            );
            self.show_notices([Notice::warn(format!("layout not saved: {err}"))]);
        } else {
            tracing::debug!(
                %session,
                groups = document.groups.len(),
                path = "arrangement",
                "arrangement save committed"
            );
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
