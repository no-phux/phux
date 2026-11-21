//! The attached session's arrangement record on an `ORGANIZATION` server.
//!
//! Correlated control reads settle the attach's layout independently of
//! the legacy metadata subscription.

use phux_protocol::ids::SessionId;

use super::{AttachError, Connection, Notice, RepaintAccumulator, SidebarReservation, Step};
use crate::attach::arrangement_link::ArrangementLink;
use crate::layout::ArrangementDoc;

impl super::SessionLoop {
    /// A failed control read is not an authoritative absent layout.
    pub(super) fn arrangement_unloaded(&self) -> bool {
        self.arrangement.as_ref().is_some_and(|link| !link.loaded())
    }

    /// Open the arrangement link for `session` on an `ORGANIZATION` server
    /// with a control dial; `true` when it owes its first read. It connects
    /// on that read, so a failure is a failed read, never the legacy key.
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

    /// Settle the initial read, or apply a subsequent arrangement change.
    pub(super) async fn settle_arrangement_read<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        repaint: &mut RepaintAccumulator,
    ) -> Result<(), AttachError> {
        if !std::mem::take(&mut self.arrangement_read_owed) {
            return Ok(());
        }
        let Some(link) = self.arrangement.as_mut() else {
            return Ok(());
        };
        let session = link.session();
        let initial = !self.layout_read_complete;
        let (document, initial) = match link.read().await {
            // A successful retry must also settle any edits held across the
            // failure, even when the stored layout is absent.
            Ok(document) => (
                document,
                initial || !self.deferred_arrangement_edits.is_empty(),
            ),
            Err(err) => {
                tracing::warn!(%session, error = %err, reason = %err, "arrangement read failed");
                self.apply_notices(
                    vec![Notice::warn(format!("layout not loaded: {err}"))],
                    repaint,
                );
                // Release the bootstrap fence only. The unread link still
                // prevents healing, replaying held edits, and saving.
                (None, initial)
            }
        };
        if document.is_none() && !initial {
            return Ok(());
        }
        let prev_rects = self.leaf_rects(sidebar);
        let focused_before = self.mirror.focused_resource.clone();
        let mut outcome = self.mirror.adopt_layout(document, initial);
        self.settle_layout_outcome(&mut outcome);
        self.record_frame_focus(focused_before);
        self.fold_frame_outcome(conn, out, sidebar, outcome, prev_rects.as_ref(), repaint)
            .await
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

    /// Write `document` as the arrangement record; `false` without a link.
    /// Nothing is written before a read succeeded (a blind compare-and-swap
    /// would replace the record whole): the edit is reported unsaved.
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
