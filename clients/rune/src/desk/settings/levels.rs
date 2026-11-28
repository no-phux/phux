//! Going into and out of Settings and its levels: opening and closing it,
//! a category and back, and which scroller each level keeps its place in.

use rune::Tx;

use super::super::Desk;
use super::Category;

/// How long after closing the stage under Settings still lands where it
/// goes at once: the page's whole exit on the page spring.
const SETTLING: f64 = 0.5;

impl Desk {
    pub fn toggle_settings(&mut self) {
        self.close_inbox();
        if self.settings_open {
            self.close_settings();
        } else {
            self.refresh_labs();
            self.settings_open = true;
        }
    }

    pub fn close_settings(&mut self) {
        if self.settings_open {
            self.settings_ui.closed_at = Some(self.clock);
        }
        self.settings_open = false;
        self.settings_ui.query.focused = false;
    }

    /// Settings is open, or leaving: the phone's stage lands where the top
    /// bar going or coming back puts it, at once, so only the page moves
    /// and nothing drifts under it.
    pub fn settings_settling(&self) -> bool {
        self.settings_open
            || (self.settings_ui.closed_at).is_some_and(|at| self.clock - at < SETTLING)
    }

    pub fn settings_category(&mut self, category: Category) {
        if category == Category::RemoteHosts && self.phone.is_none() {
            self.list_devices();
        }
        self.settings_ui.category = Some(category);
        self.settings_ui.query.clear();
        self.settings_ui.query.focused = false;
    }

    pub fn settings_back(&mut self) {
        if self.phone.is_some() && self.settings_ui.category.is_some() {
            self.settings_ui.restore_list = Some(self.settings_ui.list_scroll.get());
        }
        self.settings_ui.category = None;
        self.settings_ui.query.clear();
        self.settings_ui.query.focused = false;
    }

    /// The scroller Settings shows: a phone's level's own, else the one
    /// the desktop's panel keeps for every category.
    pub fn settings_scroller(&self) -> &'static str {
        match self.settings_ui.category {
            Some(category) if self.phone.is_some() => category.scroller(),
            _ => "settings-content",
        }
    }

    /// The phone's list laid out again after a level's back: it goes back
    /// where it was left, in the frame it shows.
    pub fn restore_settings_list(&mut self, tx: &mut Tx) {
        if let Some(offset) = self.settings_ui.restore_list.take() {
            tx.scroll_to_instant("settings-content", 0.0, offset);
        }
    }
}
