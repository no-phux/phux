//! The title bar and "Show toolbar on hover" (`[rune] title-bar`).

use phux_config_core::rune::TitleBar;
use rune::prefs::Prefs;

use crate::desk::Desk;

/// The title bar Settings saved: shown, or revealed at the top edge.
pub(super) fn saved(prefs: &Prefs) -> TitleBar {
    hover(prefs.get("hover_toolbar").unwrap_or(false))
}

fn hover(on: bool) -> TitleBar {
    if on { TitleBar::Hover } else { TitleBar::Shown }
}

impl Desk {
    /// Switch between the shown title bar and the one revealed on hover.
    pub fn toggle_hover_toolbar(&mut self) {
        if self.file_managed(self.file.title_bar.is_some()) {
            return;
        }
        let on = self.settings.title_bar != TitleBar::Hover;
        self.settings.title_bar = hover(on);
        let _ = self.prefs.set("hover_toolbar", &on);
    }
}
