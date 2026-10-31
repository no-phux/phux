//! The settings window's state: what is remembered between launches and how
//! changing it reaches the runtime.

use rune::prefs::Prefs;
use rune::{Key, KeyEvent, Tx};

use super::Desk;
use crate::grid;
use crate::theme::{self, Theme};

/// What the person chose, as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub theme: Option<&'static Theme>,
    pub notifications: bool,
    pub option_as_meta: bool,
    pub copy_on_select: bool,
    /// Ask before closing a terminal or tab with something running.
    pub confirm_close: bool,
}

impl Settings {
    pub fn load(prefs: &Prefs) -> Self {
        Self {
            theme: prefs.get::<String>("theme").and_then(|id| theme::find(&id)),
            notifications: prefs.get("notifications").unwrap_or(true),
            option_as_meta: prefs.get("option_as_meta").unwrap_or(false),
            copy_on_select: prefs.get("copy_on_select").unwrap_or(false),
            confirm_close: prefs.get("confirm_close").unwrap_or(false),
        }
    }
}

impl Desk {
    pub fn zoom(&mut self, size: Option<f32>) {
        self.font_size = size.map_or(grid::DEFAULT_FONT_SIZE, |s| {
            s.clamp(grid::MIN_FONT_SIZE, grid::MAX_FONT_SIZE)
        });
        let _ = self.prefs.set("font_size", &self.font_size);
        self.fitted.clear();
        self.fit();
    }

    pub fn toggle_sidebar(&mut self) {
        self.sidebar_open = !self.sidebar_open;
        let _ = self.prefs.set("sidebar_open", &self.sidebar_open);
    }

    pub fn toggle_settings(&mut self) {
        self.close_inbox();
        self.settings_open = !self.settings_open;
    }

    pub fn close_settings(&mut self) {
        self.settings_open = false;
    }

    /// Install the chosen palette on the runtime. Live panes repaint with it.
    pub(super) fn apply_theme(&mut self) {
        if let Some(client) = &self.client {
            client.set_terminal_theme(self.settings.theme.map(Theme::terminal));
        }
        self.generation += 1;
    }

    pub fn choose_theme(&mut self, theme: Option<&'static Theme>) {
        self.settings.theme = theme;
        let _ = self
            .prefs
            .set("theme", &theme.map_or(String::new(), |t| t.id.to_owned()));
        self.apply_theme();
    }

    pub fn toggle_notifications(&mut self) {
        self.settings.notifications = !self.settings.notifications;
        let _ = self
            .prefs
            .set("notifications", &self.settings.notifications);
    }

    pub fn toggle_option_as_meta(&mut self) {
        self.settings.option_as_meta = !self.settings.option_as_meta;
        let _ = self
            .prefs
            .set("option_as_meta", &self.settings.option_as_meta);
    }

    pub fn toggle_copy_on_select(&mut self) {
        self.settings.copy_on_select = !self.settings.copy_on_select;
        let _ = self
            .prefs
            .set("copy_on_select", &self.settings.copy_on_select);
    }

    pub fn toggle_confirm_close(&mut self) {
        self.settings.confirm_close = !self.settings.confirm_close;
        let _ = self
            .prefs
            .set("confirm_close", &self.settings.confirm_close);
    }

    /// Keys while settings is open and no control has them: arrows walk the
    /// themes, previewing live, as a focused theme grid does.
    pub(super) fn settings_key(&mut self, event: &KeyEvent, tx: &Tx) {
        if tx.focused().is_some() {
            return;
        }
        let at = self
            .settings
            .theme
            .and_then(|t| theme::THEMES.iter().position(|x| x == t))
            .map_or(0, |i| i + 1);
        let count = theme::THEMES.len() + 1;
        let step = match event.key {
            Key::Escape => return self.close_settings(),
            Key::Right | Key::Down => 1,
            Key::Left | Key::Up => count - 1,
            _ => return,
        };
        let next = (at + step) % count;
        self.choose_theme(next.checked_sub(1).map(|i| &theme::THEMES[i]));
    }

    /// Selection finished: hand it to the clipboard when asked to.
    pub fn selection_done(&mut self, tx: &mut Tx) {
        if self.settings.copy_on_select {
            self.copy(tx);
        }
    }
}
