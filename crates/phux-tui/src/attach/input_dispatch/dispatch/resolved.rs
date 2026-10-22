//! Resolved action execution, layout fences, and chord interception.

use super::{
    AttachError, ChordOutcome, DeferredArrangementEdit, EventEnv, InputEvent, StageOutcome,
    apply_action_effects, consume_chord, defers_for_layout_read, edits_workspace, run_action,
};

#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
impl<W: crate::attach::RenderSink> EventEnv<'_, '_, W> {
    /// ADR-0147: close the floating overlay, if one is open, by killing its
    /// Terminal. The slot goes at once so input and paint return to the
    /// layout without waiting for the close; `true` when one was open.
    pub(super) async fn dismiss_floating(&mut self) -> Result<bool, AttachError> {
        let Some(id) = crate::attach::floating::floating_pane(self.panes).cloned() else {
            return Ok(false);
        };
        self.panes.remove(&id);
        self.ctx.expected_closes.insert(id.clone());
        let request_id = self.ctx.take_request_id();
        self.conn
            .send(&super::super::args::kill_resource_frame(&id, request_id))
            .await?;
        Ok(true)
    }

    /// Queue a hidden-view edit until the layout read completes. With no
    /// queue the keystroke is dropped: the fallback workspace is never edited.
    pub(super) fn defer_arrangement_edit(&mut self, action: &str) {
        let edit = match action {
            "close-view" => {
                let Some(target) = self.focused_resource.clone() else {
                    tracing::info!(
                        action = "close-view",
                        reason = "no focused view",
                        "arrangement save skipped"
                    );
                    return;
                };
                DeferredArrangementEdit::CloseView { target }
            }
            "reopen-views" => DeferredArrangementEdit::ReopenViews,
            _ => return,
        };
        if let Some(queue) = self.ctx.deferred_arrangement_edits.as_deref_mut() {
            tracing::info!(
                action,
                reason = "layout read fence",
                "arrangement save deferred"
            );
            queue.push(edit);
            if let Some(owed) = self.ctx.unloaded_arrangement_read.as_deref_mut() {
                *owed = true;
            }
        } else {
            tracing::info!(
                action,
                reason = "layout read fence",
                "arrangement save skipped"
            );
        }
    }

    /// Run an action through the path every trigger shares; true iff the
    /// action or floating overlay dismissal needs a full repaint.
    pub(super) async fn run_resolved(
        &mut self,
        resolved: &phux_config::keybind::ResolvedAction,
    ) -> Result<bool, AttachError> {
        // ADR-0147: any action first dismisses the floating overlay; for
        // `kill-pane` that is the whole action.
        let dismissed = self.dismiss_floating().await?;
        if dismissed && resolved.action == "kill-pane" {
            return Ok(true);
        }
        if !self.ctx.layout_read_complete && edits_workspace(&resolved.action) {
            tracing::info!(
                action = %resolved.action,
                reason = "layout read fence",
                "arrangement save skipped"
            );
            return Ok(dismissed);
        }
        // #1475: these two edit the arrangement but were not in the fence
        // list, so they mutated the fallback layout and the save was then
        // dropped. Hold them until the read adopts the stored arrangement.
        if defers_for_layout_read(&resolved.action, self.ctx) {
            self.defer_arrangement_edit(&resolved.action);
            return Ok(dismissed);
        }
        let effects = run_action(
            resolved,
            self.ctx,
            self.focused_resource.as_ref(),
            self.panes,
        );
        Ok(apply_action_effects(
            effects,
            self.out,
            self.conn,
            self.ctx,
            self.focused_resource,
            self.predict,
            self.panes,
        )
        .await?
            || dismissed)
    }
    /// Resolver intercept. Runs BEFORE the predict layer
    /// so a chord that resolves to e.g. `focus-direction` doesn't
    /// leave a stale ghost overlay on the previous focused pane.
    pub(super) async fn intercept_chord(
        &mut self,
        ev: &InputEvent,
    ) -> Result<StageOutcome, AttachError> {
        let InputEvent::Key(key_event) = ev else {
            return Ok(StageOutcome::PASS);
        };
        let Some(outcome) = consume_chord(self.ctx, key_event) else {
            return Ok(StageOutcome::PASS);
        };
        match outcome {
            // Still waiting on the next chord in a multi-chord
            // sequence, or an exclusive key mode ate the key: absorb it.
            ChordOutcome::Partial | ChordOutcome::Swallowed => Ok(StageOutcome::CONSUMED),
            ChordOutcome::Resolved(resolved) => {
                let layout_changed = self.run_resolved(&resolved).await?;
                Ok(StageOutcome::consumed(layout_changed))
            }
        }
    }
}
