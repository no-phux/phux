//! Timer wakes: input-origin badge fades, the sync-output watchdog, and the
//! which-key popup.

use super::super::{
    RepaintLevel, SYNC_OUTPUT_WATCHDOG, SidebarReservation, push_which_key_overlay,
};

impl super::super::SessionLoop {
    /// The next fade boundary of any origin badge, on the tokio clock.
    pub(in crate::attach::driver::loop_state) fn next_input_origin_wake(
        &self,
    ) -> Option<tokio::time::Instant> {
        let now = std::time::Instant::now();
        let soonest = self
            .mirror
            .panes
            .values()
            .filter_map(|slot| slot.input_origin_delay(now))
            .min()?;
        Some(tokio::time::Instant::now() + soonest)
    }

    /// Repaint the rail at a fade boundary, and drop a badge that has ended.
    pub(in crate::attach::driver::loop_state) fn on_input_origin_phase<
        W: crate::attach::RenderSink,
    >(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        let now = std::time::Instant::now();
        let mut dirty = false;
        for slot in self.mirror.panes.values_mut() {
            let Some(mark) = slot.input_origin.as_ref() else {
                continue;
            };
            dirty = true;
            if now.saturating_duration_since(mark.since)
                >= crate::attach::pane_state::INPUT_ORIGIN_LIFE
            {
                slot.input_origin = None;
            }
        }
        if dirty {
            self.repaint_view(out, sidebar, RepaintLevel::Chrome);
        }
    }

    /// An application that never sent `?2026l`: expose the mirror once.
    pub(in crate::attach::driver::loop_state) fn on_sync_output_timeout<
        W: crate::attach::RenderSink,
    >(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        let now = tokio::time::Instant::now();
        let mut expired = false;
        for slot in self.mirror.panes.values_mut() {
            if slot.sync_output_dirty
                && slot.sync_output_since.is_some_and(|since| {
                    now.saturating_duration_since(since) >= SYNC_OUTPUT_WATCHDOG
                })
            {
                slot.sync_output_since = None;
                slot.sync_output_dirty = false;
                expired = true;
            }
        }
        if expired {
            self.repaint_view(out, sidebar, RepaintLevel::Full);
        }
    }

    /// Push the which-key popup listing the pending prefix's continuations.
    pub(in crate::attach::driver::loop_state) fn on_which_key_timeout<
        W: crate::attach::RenderSink,
    >(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        self.which_key_deadline = None;
        if push_which_key_overlay(
            &mut self.overlays,
            &self.settings.resolver,
            &self.settings.keybindings,
            &self.settings.theme,
        ) {
            self.paint_overlay(out, sidebar);
        }
    }
}
