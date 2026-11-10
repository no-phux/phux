//! Heal kitty images the outer terminal may have dropped, on the loop's
//! housekeeping timer it shares with agent notifications.

use super::super::{RepaintLevel, SidebarReservation};
use crate::attach::pane_state::{heal_kitty, kitty_heal_deadline};

impl super::super::SessionLoop {
    /// The earlier of the next agent notification and the next kitty heal.
    pub(in crate::attach::driver::loop_state) fn housekeeping_deadline(
        &self,
    ) -> Option<tokio::time::Instant> {
        [
            self.notifier.deadline(),
            kitty_heal_deadline(&self.mirror.panes),
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
        let now = tokio::time::Instant::now();
        if kitty_heal_deadline(&self.mirror.panes).is_some_and(|at| at <= now) {
            self.heal_kitty_images(out, sidebar);
        }
    }

    /// Forget what the outer terminal holds of every pane's kitty images and
    /// repaint the visible panes' rows in place, which re-sends the images
    /// and leaves clean cells alone. Under an overlay or a held switch the
    /// next paint re-sends them.
    pub(in crate::attach::driver::loop_state) fn heal_kitty_images<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        heal_kitty(&mut self.mirror.panes);
        self.repaint_view(out, sidebar, RepaintLevel::Rows);
    }
}
