//! Event application, damage accounting, and publication catch-up.

#[cfg(feature = "engine")]
use super::{Arc, EngineError, FrameKey, GridFrame, ViewPresentation};
use super::{
    EngineApplyError, EngineEvent, EngineOutcome, InputBlockReason, InputEligibility,
    KernelDamageKind, KernelEffect, Owner, ResourceId, apply_event,
};

impl Owner {
    pub(super) fn is_closed(&self, id: &ResourceId) -> bool {
        matches!(
            self.kernel.input_eligibility(id),
            InputEligibility::Ineligible(InputBlockReason::Closed)
        )
    }

    pub(super) fn apply_batch(&mut self, events: Vec<EngineEvent>) -> Vec<EngineOutcome> {
        crate::perf::APPLY_BATCHES.incr();
        let _timed = crate::perf::APPLY_EXECUTE.timer();
        crate::perf::APPLY_EVENTS.record(u64::try_from(events.len()).unwrap_or(u64::MAX));
        let mut damaged = Vec::new();
        let mut outcomes = Vec::with_capacity(events.len());
        for event in events {
            let outcome = self.apply_one(event, &mut damaged);
            if outcome.error.is_some() {
                crate::perf::APPLY_ERRORS.incr();
            }
            let fatal = outcome.resync_required()
                || matches!(outcome.error.as_ref(), Some(EngineApplyError::Protocol(_)));
            outcomes.push(outcome);
            if fatal {
                break;
            }
        }
        #[cfg(feature = "engine")]
        self.publish_damaged(damaged);
        #[cfg(not(feature = "engine"))]
        drop(damaged);
        outcomes
    }

    fn apply_one(&mut self, event: EngineEvent, damaged: &mut Vec<ResourceId>) -> EngineOutcome {
        // A frame queued behind a terminal close is stale evidence, including
        // when the close and stale frame share this batch.
        if event
            .terminal_id()
            .is_some_and(|terminal_id| self.is_closed(terminal_id))
        {
            return EngineOutcome::default();
        }
        if let EngineEvent::AttachStarted { terminals, .. } = &event {
            self.prepare_attach(terminals);
        }
        #[cfg(feature = "engine")]
        let closing = match &event {
            EngineEvent::Closed { terminal_id, .. } => Some(terminal_id.clone()),
            _ => None,
        };
        #[cfg(feature = "engine")]
        let output = match &event {
            EngineEvent::Output { terminal_id, .. } => Some(terminal_id.clone()),
            _ => None,
        };
        let result = apply_event(&mut self.kernel, event, &mut self.effects);
        #[cfg(feature = "engine")]
        if result.is_ok()
            && let Some(id) = output
        {
            self.note_view_output(&id);
        }
        let effects = self.effects.take();
        // A retained close moves the final replica aside before the damage
        // walk sees the kernel's `Removed`, so the walk re-publishes it
        // from there instead of dropping its slot.
        #[cfg(feature = "engine")]
        if let Some(id) = closing {
            self.capture_closed(id);
        }
        self.note_damage(&effects, damaged);
        EngineOutcome {
            effects,
            error: result.as_ref().err().map(EngineApplyError::from_kernel),
        }
    }

    /// Theme every published replica and every later bootstrap, then
    /// republish the visible terminals. Without the libghostty engine there
    /// are no colours to set.
    #[cfg_attr(
        not(feature = "engine"),
        allow(
            clippy::unused_self,
            clippy::needless_pass_by_ref_mut,
            clippy::missing_const_for_fn
        )
    )]
    pub(super) fn set_theme(&mut self, theme: Option<super::TerminalTheme>) {
        #[cfg(feature = "engine")]
        {
            let rgb = |[r, g, b]: [u8; 3]| libghostty_vt::style::RgbColor { r, g, b };
            let theme = theme.map(|theme| phux_client_core::engine::ghostty::TerminalTheme {
                ansi16: theme.ansi16.map(rgb),
                foreground: rgb(theme.foreground),
                background: rgb(theme.background),
                cursor: rgb(theme.cursor),
            });
            self.kernel.adapter_mut().set_theme(theme);
            let visible: Vec<_> = self.visible.iter().cloned().collect();
            for id in &visible {
                if let Some(replica) = self.kernel.published_engine_mut(id)
                    && let Err(error) = replica.apply_theme(theme.as_ref())
                {
                    tracing::warn!(terminal = %id, %error, "terminal theme not applied");
                }
            }
            self.publish_damaged(visible);
        }
        #[cfg(not(feature = "engine"))]
        let _ = theme;
    }

    #[cfg(feature = "engine")]
    fn publish_damaged(&mut self, damaged: Vec<ResourceId>) {
        self.release_pending();
        for id in damaged {
            if let Err(error) = self.render_damage(&id) {
                tracing::warn!(terminal = %id, %error, "grid projection failed");
            }
        }
    }

    /// Project a presentation an acquire found deferred and hand its frame
    /// to that consumer. A presentation that no longer exists hands back
    /// whatever its slot still holds.
    #[cfg(feature = "engine")]
    pub(super) fn catch_up(&mut self, key: &FrameKey) -> Option<Arc<GridFrame>> {
        let _timed = crate::perf::CATCH_UP_EXECUTE.timer();
        let result = match key {
            FrameKey::Default(id) => self.render_and_publish(id).map(drop),
            FrameKey::View(view) if self.views.contains_key(view) => {
                self.with_view(*view, Self::render_and_publish).map(drop)
            }
            FrameKey::View(_) => Ok(()),
        };
        match result {
            Ok(()) => crate::perf::CAUGHT_UP.incr(),
            Err(error) => tracing::warn!(?key, %error, "grid catch-up failed"),
        }
        self.publication.hand_off(key)
    }

    /// The active presentation's replica facts and frame, caught up first
    /// when its projection was deferred, read in this one turn so the frame
    /// is exactly the replica's state. Marks the frame read, as an acquire
    /// does.
    #[cfg(feature = "engine")]
    pub(super) fn present(&mut self, id: &ResourceId) -> Result<ViewPresentation, EngineError> {
        let key = self
            .active_view
            .map_or_else(|| FrameKey::Default(id.clone()), FrameKey::View);
        crate::perf::ACQUIRED.incr();
        if self.publication.stale(&key) {
            let _timed = crate::perf::CATCH_UP_EXECUTE.timer();
            self.render_and_publish(id)?;
            crate::perf::CAUGHT_UP.incr();
        }
        Ok(ViewPresentation {
            replica: self.replica_info(id)?,
            frame: self.publication.hand_off(&key),
        })
    }

    /// Record which terminals gained or lost a projection in effect order,
    /// deduplicating across the whole owner-thread batch.
    fn note_damage(&mut self, effects: &[KernelEffect], damaged: &mut Vec<ResourceId>) {
        for effect in effects {
            #[cfg(feature = "engine")]
            self.note_history_damage(effect, damaged);
            let KernelEffect::Damage(damage) = effect else {
                continue;
            };
            let id = &damage.terminal_id;
            if damage.kind == KernelDamageKind::Removed {
                self.visible.remove(id);
                damaged.retain(|damaged| damaged != id);
                #[cfg(feature = "engine")]
                if self.closed.contains_key(id) {
                    // Retained: the final frame is projected from the
                    // closed replica.
                    damaged.push(id.clone());
                } else {
                    self.projectors.remove(id);
                    self.publication.remove(id);
                    self.forget_views(id);
                    self.invalidate_handles(id);
                    self.presentations.remove(id);
                }
            } else {
                self.visible.insert(id.clone());
                #[cfg(feature = "engine")]
                if let Err(error) = self.bump_document_revision(id) {
                    tracing::warn!(terminal = %id, %error, "document revision could not advance");
                }
                if !damaged.contains(id) {
                    damaged.push(id.clone());
                }
            }
        }
    }

    fn prepare_attach(&mut self, terminals: &[ResourceId]) {
        self.kernel.release_active_attach();
        for terminal_id in terminals {
            if !self.is_closed(terminal_id) {
                continue;
            }
            self.visible.remove(terminal_id);
            #[cfg(feature = "engine")]
            {
                self.closed.remove(terminal_id);
                self.pending_releases.remove(terminal_id);
            }
            let _ = self.kernel.release_terminal(terminal_id);
        }
    }
}
