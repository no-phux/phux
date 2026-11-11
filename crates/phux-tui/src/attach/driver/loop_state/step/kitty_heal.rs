//! Heal kitty images the outer terminal may have dropped (see
//! `attach::kitty_heal`), on the loop's housekeeping timer it shares with
//! agent notifications and on outer focus-in.

use std::time::Instant;

use super::super::{RepaintLevel, SidebarReservation};

impl super::super::SessionLoop {
    /// The earlier of the next agent notification and the next kitty heal.
    pub(in crate::attach::driver::loop_state) fn housekeeping_deadline(
        &self,
    ) -> Option<tokio::time::Instant> {
        let heal = self.mirror.kitty_heal.deadline(&self.mirror.panes);
        [
            self.notifier.deadline(),
            heal.map(tokio::time::Instant::from_std),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Run whichever of the notifier and the kitty heal came due.
    pub(in crate::attach::driver::loop_state) fn on_housekeeping_due<
        W: crate::attach::RenderSink,
    >(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        self.on_notify_due(out);
        self.heal_kitty_images(out, sidebar, Instant::now(), false);
    }

    /// The outer terminal reported its focus; regaining it heals images that
    /// may have gone while it was elsewhere.
    pub(in crate::attach::driver::loop_state) fn on_host_focus<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        focused: bool,
    ) {
        self.mirror.kitty_heal.note_focus(focused);
        if focused {
            self.heal_kitty_images(out, sidebar, Instant::now(), true);
        }
    }

    /// Mark the images due a heal at `now` (the focus-in set when
    /// `on_focus`) for re-sending and repaint the visible panes' rows in
    /// place, which re-sends them and leaves clean cells alone. Under an
    /// overlay or a held switch the heal is deferred instead.
    pub(in crate::attach::driver::loop_state) fn heal_kitty_images<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        now: Instant,
        on_focus: bool,
    ) {
        if self.overlays.is_active() || self.paint_held() {
            self.mirror.kitty_heal.defer(now);
            return;
        }
        let mirror = &mut self.mirror;
        let healed = if on_focus {
            mirror.kitty_heal.heal_on_focus(&mut mirror.panes, now)
        } else {
            mirror.kitty_heal.heal_due(&mut mirror.panes, now)
        };
        if healed {
            self.repaint_view(out, sidebar, RepaintLevel::Rows);
        }
    }
}
