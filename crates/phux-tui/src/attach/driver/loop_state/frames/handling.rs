//! Frame application and initial layout settlement.

use super::super::{
    AttachError, FrameEnv, FrameKind, FrameOutcome, ResourceId, SidebarReservation,
    handle_server_frame, undelivered_notices,
};

impl super::super::SessionLoop {
    /// Hand one inbound frame to the shared server-frame handler.
    pub(in crate::attach::driver::loop_state) fn handle_frame<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        frame: FrameKind,
        sidebar: Option<SidebarReservation>,
        defer_paint: bool,
    ) -> Result<FrameOutcome, AttachError> {
        let retired_terminal = match &frame {
            FrameKind::ResourceClosed { terminal_id, .. } => Some(terminal_id.clone()),
            _ => None,
        };
        // A held switch defers every pane paint to the hold's release.
        let defer_paint = defer_paint || self.paint_held();
        let env = FrameEnv {
            focused_session: self.peers.focused_session,
            status_bar: self.settings.status_bar.as_mut(),
            sidebar,
            viewport_dims: self.viewport_dims,
            pending_layout_request: self.layout_get_request_id,
            overlay_active: self.overlays.is_active(),
            defer_paint,
        };
        let mut outcome = handle_server_frame(&mut self.mirror, env, out, frame)?;
        self.deliver_clipboard_writes(out, &mut outcome)?;
        for terminal_id in &outcome.authoritative_damage {
            let fenced = self
                .input_replay
                .as_ref()
                .is_some_and(|journal| journal.borrow().delivery_fenced(terminal_id));
            if fenced {
                self.delivery_fence_paint_pending
                    .insert(terminal_id.clone());
                if outcome.painted_output.as_ref() != Some(terminal_id) {
                    self.pacer.withhold(terminal_id);
                }
            }
        }
        if let Some(terminal_id) = outcome.painted_output.as_ref() {
            self.clear_delivery_fence_after_paint(terminal_id);
        }
        self.retire_closed_terminal_input(retired_terminal, &mut outcome);
        self.settle_layout_outcome(&mut outcome);
        Ok(outcome)
    }

    /// Retire replay state only for the terminal the server actually closed.
    fn retire_closed_terminal_input(
        &mut self,
        terminal_id: Option<ResourceId>,
        outcome: &mut FrameOutcome,
    ) {
        if let Some(terminal_id) = terminal_id {
            self.review.forget(&terminal_id);
            self.delivery_fence_paint_pending.remove(&terminal_id);
            if let Some(journal) = self.input_replay.as_ref() {
                let reports = journal
                    .borrow_mut()
                    .retire_terminal(&terminal_id, "the terminal closed");
                outcome.notices.extend(undelivered_notices(reports));
            }
        }
    }

    /// Adopt the initial read and replay held edits only onto a loaded layout.
    /// A successful retry must settle edits retained across an earlier failure.
    fn settle_layout_outcome(&mut self, outcome: &mut FrameOutcome) {
        if outcome.layout_get_answered {
            self.layout_read_complete = true;
            self.layout_get_request_id = None;
            self.heal_initial_layout(outcome);
        } else if outcome.layout_get_refused {
            // Settled, but unread: the fence stays and nothing is healed.
            self.layout_get_request_id = None;
        }
        if (outcome.layout_get_answered || outcome.layout_replaced)
            && self.flush_deferred_arrangement_edits()
        {
            outcome.layout_replaced = true;
            outcome.emit_set_metadata = true;
        }
    }

    /// Legacy or absent layouts can seed missing members; an unread record
    /// is not absent, and an existing organization record keeps views hidden.
    fn heal_initial_layout(&mut self, outcome: &mut FrameOutcome) {
        // #1207: heal a missing or partial key instead of hiding panes,
        // and save it so every client and `phux insert-pane` agree.
        // On an `ORGANIZATION` server the layout is the session's
        // arrangement record, and a member it leaves out was closed on
        // purpose: it stays unarranged behind the reopen action (R15).
        // Only an absent record, or a legacy key, is healed. A record
        // that could not be read is not absent: healing it would save a
        // default over the stored arrangement, so it is left alone.
        let unloaded = self.arrangement.as_ref().is_some_and(|link| !link.loaded());
        let key_absent = !outcome.layout_replaced && !unloaded;
        let heal = key_absent || !(self.organization_supported || unloaded);
        if heal && self.place_unplaced_panes() {
            outcome.layout_replaced = true;
            outcome.emit_set_metadata = true;
        } else if key_absent && !self.mirror.workspace.groups.is_empty() {
            outcome.emit_set_metadata = true;
        }
    }

    /// Apply hidden-view edits queued while the layout read was outstanding.
    /// `true` when the adopted workspace changed and must be saved.
    pub(in crate::attach::driver::loop_state) fn flush_deferred_arrangement_edits(
        &mut self,
    ) -> bool {
        if !self.layout_read_complete || self.arrangement_unloaded() {
            return false;
        }
        let mut next_focus = self.mirror.focused_resource.clone();
        let changed = crate::attach::input_dispatch::apply_deferred_arrangement_edits(
            &mut self.deferred_arrangement_edits,
            &mut self.mirror.workspace,
            &self.mirror.members,
            &self.mirror.panes,
            &mut self.mirror.zoomed,
            &mut next_focus,
        );
        if changed {
            self.focus_history
                .transition(&mut self.mirror.focused_resource, next_focus);
            self.mirror.predict.suspend();
            if let Some(focused) = self.mirror.focused_resource.as_ref() {
                super::super::reanchor_predict_to_pane(
                    &mut self.mirror.predict,
                    &self.mirror.panes,
                    focused,
                );
            }
        }
        changed
    }
}
