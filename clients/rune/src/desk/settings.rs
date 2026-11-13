//! The settings window's state: what is remembered between launches and how
//! changing it reaches the runtime.

use phux_config_core::RuneCfg;
use rune::prefs::Prefs;
use rune::{Key, KeyEvent, Tx};

use super::{Desk, NoticeKind};
use crate::grid;
use crate::theme::{self, Theme};

/// Settings destinations, separate from preferences and connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Appearance,
    Terminal,
    Notifications,
    RemoteHosts,
    Developer,
    About,
}

impl Category {
    pub const ALL: [Self; 6] = [
        Self::Appearance,
        Self::Terminal,
        Self::Notifications,
        Self::RemoteHosts,
        Self::Developer,
        Self::About,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Terminal => "Terminal",
            Self::Notifications => "Notifications",
            Self::RemoteHosts => "Remote Hosts",
            Self::Developer => "Developer",
            Self::About => "About & Updates",
        }
    }

    pub const fn key(self) -> &'static str {
        match self {
            Self::Appearance => "settings-appearance",
            Self::Terminal => "settings-terminal",
            Self::Notifications => "settings-notifications",
            Self::RemoteHosts => "settings-hosts",
            Self::Developer => "settings-developer",
            Self::About => "settings-about",
        }
    }
}

#[derive(Default)]
pub struct SettingsUi {
    pub category: Option<Category>,
    pub query: rune::TextEdit,
    /// The existing theme palette returns here on both accept and cancel.
    pub return_from_themes: bool,
}

/// What the person chose, as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub theme: Option<&'static Theme>,
    pub notifications: bool,
    pub option_as_meta: bool,
    pub copy_on_select: bool,
    /// Ask before closing a terminal or tab with something running.
    pub confirm_close: bool,
    /// How much Liquid Glass shows through the window chrome, 0 to 100.
    pub glass: u8,
}

impl Settings {
    pub fn load(prefs: &Prefs) -> Self {
        Self {
            // Never chosen: Omarchy's theme where there is one. An empty id
            // is System, picked on purpose.
            theme: match prefs.get::<String>("theme") {
                None => theme::omarchy(),
                Some(id) => theme::find(&id),
            },
            notifications: prefs.get("notifications").unwrap_or(true),
            option_as_meta: prefs.get("option_as_meta").unwrap_or(false),
            copy_on_select: prefs.get("copy_on_select").unwrap_or(false),
            confirm_close: prefs.get("confirm_close").unwrap_or(false),
            glass: prefs.get::<u8>("glass").unwrap_or(0).min(100),
        }
    }

    /// These settings with `config.toml`'s `[rune]` keys laid over them.
    pub fn over(self, file: &RuneCfg) -> Self {
        Self {
            theme: file
                .theme
                .as_deref()
                .map_or(self.theme, |id| theme::find(id)),
            notifications: file.notifications.unwrap_or(self.notifications),
            option_as_meta: file.option_as_meta.unwrap_or(self.option_as_meta),
            copy_on_select: file.copy_on_select.unwrap_or(self.copy_on_select),
            confirm_close: file.confirm_close.unwrap_or(self.confirm_close),
            glass: self.glass,
        }
    }
}

/// `config.toml`'s `[rune]` table, and what was wrong with the file if it
/// did not load; its `[[sessions]]` colours are learned on the way. Tests
/// never read the real file.
pub(super) fn load_file() -> (RuneCfg, Option<String>) {
    if cfg!(test) {
        return (RuneCfg::default(), None);
    }
    match phux_config_core::loader::load_client() {
        Ok(config) => {
            crate::session_color::learn(config.sessions);
            if let Some(socket) = config.rune.ssh_agent.as_deref().map(str::trim) {
                let home = std::env::var_os("HOME").unwrap_or_default();
                let socket = socket.strip_prefix("~/").map_or_else(
                    || socket.into(),
                    |rest| std::path::Path::new(&home).join(rest),
                );
                phux_client_runtime::connection::set_ssh_agent(socket);
            }
            (config.rune, None)
        }
        Err(err) => (RuneCfg::default(), Some(err.to_string())),
    }
}

impl Desk {
    /// True, with a word on why, when `config.toml` owns a setting.
    fn file_managed(&mut self, set: bool) -> bool {
        if set {
            self.say(
                NoticeKind::Info,
                "Set in config.toml. Edit the file to change it.",
            );
        }
        set
    }

    /// What is chosen now, as a `[rune]` block for `config.toml`.
    pub fn copy_config(&mut self, tx: &mut Tx) {
        let settings = self.settings;
        tx.copy(
            RuneCfg {
                theme: Some(settings.theme.map_or(String::new(), |t| t.id.to_owned())),
                font_size: Some(self.font_size),
                font_family: Some(self.font_family.clone()),
                notifications: Some(settings.notifications),
                option_as_meta: Some(settings.option_as_meta),
                copy_on_select: Some(settings.copy_on_select),
                confirm_close: Some(settings.confirm_close),
                ..RuneCfg::default()
            }
            .to_toml(),
        );
        self.say(NoticeKind::Info, "Copied settings as a [rune] table.");
    }

    // A file's `font-size` is where each window starts; zooming still works.
    pub fn zoom(&mut self, size: Option<f32>) {
        self.font_size = size.map_or(grid::DEFAULT_FONT_SIZE, |s| {
            s.clamp(grid::MIN_FONT_SIZE, grid::MAX_FONT_SIZE)
        });
        let _ = self.prefs.set("font_size", &self.font_size);
        self.fitted.clear();
        self.fit();
    }

    /// Step to the next family in `grid::FONT_CHOICES`; one from prefs
    /// that isn't listed steps to the default.
    pub fn next_font(&mut self) {
        if self.file_managed(self.file.font_family.is_some()) {
            return;
        }
        let at = grid::FONT_CHOICES
            .iter()
            .position(|f| *f == self.font_family);
        let next = at.map_or(0, |i| (i + 1) % grid::FONT_CHOICES.len());
        self.font_family = grid::FONT_CHOICES[next].to_owned();
        let _ = self.prefs.set("font_family", &self.font_family);
        self.font_due = true;
    }

    /// Put a chosen family behind the mono slot, then refit the panes once
    /// Rune measures in it (the frame after).
    pub(super) fn follow_font(&mut self, tx: &mut Tx) {
        if std::mem::take(&mut self.font_due) {
            tx.set_font_families(grid::FAMILIES.0, grid::mono_family(&self.font_family));
            tx.wake_after(std::time::Duration::ZERO);
        }
        let epoch = rune::text::font_epoch();
        if std::mem::replace(&mut self.font_epoch, epoch) != epoch {
            self.fit();
        }
    }

    pub fn toggle_sidebar(&mut self) {
        // A narrow window folds the sidebar on its own; the toggle peeks
        // without overwriting the preference a wider window restores.
        if self.sidebar_narrow() {
            self.sidebar_peek = !self.sidebar_peek;
            return;
        }
        self.set_sidebar_open(!self.sidebar_open);
    }

    /// The saved preference, which a wide enough window shows.
    pub fn set_sidebar_open(&mut self, open: bool) {
        self.sidebar_open = open;
        let _ = self.prefs.set("sidebar_open", &self.sidebar_open);
    }

    /// Whether the window is too narrow for the sidebar beside a usable
    /// terminal. Unknown (before the first layout) counts as wide.
    pub fn sidebar_narrow(&self) -> bool {
        const MIN_STAGE: f32 = 420.0;
        self.window_w > 0.0 && self.window_w < crate::ui::SIDEBAR_W + MIN_STAGE
    }

    /// Whether the sidebar is drawn.
    pub fn sidebar_shown(&self) -> bool {
        if self.sidebar_narrow() {
            self.sidebar_peek
        } else {
            self.sidebar_open
        }
    }

    /// The root's layout: learn the window width, and post a frame when it
    /// crosses the narrow line, since on_layout runs after poll.
    pub fn window_laid_out(&mut self, width: f32) {
        let was = self.sidebar_narrow();
        self.window_w = width;
        if was != self.sidebar_narrow() {
            self.sidebar_peek = false;
            if let Some(waker) = &self.waker {
                waker();
            }
        }
    }

    pub fn toggle_settings(&mut self) {
        self.close_inbox();
        if self.settings_open {
            self.close_settings();
        } else {
            self.settings_open = true;
        }
    }

    pub fn close_settings(&mut self) {
        self.settings_open = false;
        self.settings_ui.query.focused = false;
    }

    pub fn settings_category(&mut self, category: Category) {
        self.settings_ui.category = Some(category);
        self.settings_ui.query.clear();
        self.settings_ui.query.focused = false;
    }

    pub fn settings_back(&mut self) {
        self.settings_ui.category = None;
        self.settings_ui.query.clear();
        self.settings_ui.query.focused = false;
    }

    pub fn clear_settings_search(&mut self) {
        self.settings_ui.query.clear();
        self.focus_settings_search();
    }

    pub fn focus_settings_search(&mut self) {
        self.settings_ui.query.focused = true;
    }

    pub fn blur_settings_search(&mut self, tx: &mut Tx) {
        self.settings_ui.query.focused = false;
        tx.blur();
    }

    pub fn settings_themes(&mut self) {
        self.open_themes();
        self.settings_ui.return_from_themes = true;
    }

    pub fn settings_search_focused(&self) -> bool {
        self.settings_open && self.settings_ui.query.focused
    }

    /// Native fields keep editing, selection, clipboard and Tab with Rune.
    pub fn settings_field_key(&mut self, event: &KeyEvent, tx: &mut Tx) -> bool {
        match event.key {
            Key::Escape if !self.settings_ui.query.text.is_empty() => self.clear_settings_search(),
            Key::Escape => self.settings_dismiss(),
            Key::Enter => self.blur_settings_search(tx),
            _ => return false,
        }
        if !self.settings_search_focused() {
            tx.blur();
        }
        true
    }

    fn settings_dismiss(&mut self) {
        if self.phone.is_some() && self.settings_ui.category.is_some() {
            self.settings_back();
        } else {
            self.close_settings();
        }
    }

    pub fn settings_escape(&mut self) {
        if self.settings_ui.query.text.is_empty() {
            self.settings_dismiss();
        } else {
            self.clear_settings_search();
        }
    }

    /// Drawn keys edit this field now, never queued into a later frame where
    /// the sheet may have closed and the terminal could have the keyboard.
    pub fn settings_soft(&mut self, event: &KeyEvent, tx: &mut Tx) {
        if !self.settings_search_focused() || self.settings_field_key(event, tx) {
            return;
        }
        if event.key == Key::Tab {
            self.blur_settings_search(tx);
            return;
        }
        self.settings_ui.query.key(event, None);
    }

    /// Install the chosen palette on the runtime. Live panes repaint with it.
    pub(super) fn apply_theme(&mut self) {
        if let Some(client) = &self.client {
            client.set_terminal_theme(self.settings.theme.map(Theme::terminal));
        }
        self.generation += 1;
    }

    /// Omarchy changed its theme: a window following it repaints.
    pub(super) fn follow_omarchy(&mut self) -> bool {
        let Some(shown) = self.settings.theme.filter(|t| t.id == "omarchy") else {
            return false;
        };
        let Some(fresh) = theme::omarchy().filter(|fresh| !std::ptr::eq(*fresh, shown)) else {
            return false;
        };
        self.settings.theme = Some(fresh);
        self.apply_theme();
        true
    }

    /// Glass behind the chrome, 0 (solid) to 100 percent, applied live.
    pub fn set_glass(&mut self, percent: u8) {
        self.settings.glass = percent.min(100);
        let _ = self.prefs.set("glass", &self.settings.glass);
    }

    /// The glass fraction to draw: macOS only, where the window is
    /// transparent and the system draws Liquid Glass behind it.
    pub fn glass(&self) -> f32 {
        if cfg!(target_os = "macos") {
            f32::from(self.settings.glass) / 100.0
        } else {
            0.0
        }
    }

    pub fn choose_theme(&mut self, theme: Option<&'static Theme>) {
        if self.file_managed(self.file.theme.is_some()) {
            return;
        }
        self.settings.theme = theme;
        let _ = self
            .prefs
            .set("theme", &theme.map_or(String::new(), |t| t.id.to_owned()));
        self.apply_theme();
    }

    pub fn toggle_notifications(&mut self) {
        if self.file_managed(self.file.notifications.is_some()) {
            return;
        }
        self.settings.notifications = !self.settings.notifications;
        let _ = self
            .prefs
            .set("notifications", &self.settings.notifications);
    }

    pub fn toggle_option_as_meta(&mut self) {
        if self.file_managed(self.file.option_as_meta.is_some()) {
            return;
        }
        self.settings.option_as_meta = !self.settings.option_as_meta;
        let _ = self
            .prefs
            .set("option_as_meta", &self.settings.option_as_meta);
    }

    pub fn toggle_copy_on_select(&mut self) {
        if self.file_managed(self.file.copy_on_select.is_some()) {
            return;
        }
        self.settings.copy_on_select = !self.settings.copy_on_select;
        let _ = self
            .prefs
            .set("copy_on_select", &self.settings.copy_on_select);
    }

    pub fn toggle_confirm_close(&mut self) {
        if self.file_managed(self.file.confirm_close.is_some()) {
            return;
        }
        self.settings.confirm_close = !self.settings.confirm_close;
        let _ = self
            .prefs
            .set("confirm_close", &self.settings.confirm_close);
    }

    /// Unhandled arrows never mutate preferences. Escape backs out of the
    /// phone category before closing the sheet.
    pub(super) fn settings_key(&mut self, event: &KeyEvent, tx: &mut Tx) {
        if self.settings_search_focused() && tx.focused().is_none() {
            self.settings_soft(event, tx);
        } else if event.key == Key::Escape {
            self.settings_dismiss();
        } else if !self.settings_ui.query.focused && starts_search(event) {
            // Typing anywhere but a field searches, as System Settings does:
            // a focused button (the gear that opened the sheet) takes no text.
            self.focus_settings_search();
            self.settings_soft(event, tx);
        }
    }

    /// Selection finished: hand it to the clipboard when asked to.
    pub fn selection_done(&mut self, tx: &mut Tx) {
        if self.settings.copy_on_select {
            self.copy(tx);
        }
    }
}

/// A printable key with no chord modifier: the start of a search.
fn starts_search(event: &KeyEvent) -> bool {
    let chord = event.mods.cmd || event.mods.ctrl || event.mods.alt || event.mods.logo;
    matches!(&event.key, Key::Text(t) if !chord && !t.chars().any(char::is_control))
}

#[cfg(test)]
impl Desk {
    /// An isolated window for UI interaction/render tests, including actions
    /// that persist preferences. Never let an acceptance render write real prefs.
    pub(crate) fn settings_test_fixture() -> Self {
        let resolved = crate::launch::Resolved::offline("settings layout");
        let mut desk = Self::new(&resolved);
        desk.prefs = Prefs::memory();
        desk.settings = Settings::load(&desk.prefs);
        desk.updater = super::updates::Updater::new(&desk.prefs, &resolved);
        desk.font_size = grid::DEFAULT_FONT_SIZE;
        desk.font_family = String::new();
        desk.settings_open = true;
        desk
    }
}

#[cfg(test)]
mod tests;
