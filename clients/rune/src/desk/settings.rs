//! The settings window's state: what is remembered between launches and how
//! changing it reaches the runtime.

use phux_config_core::rune::{TabBar, TitleBar};
use phux_config_core::{ClientConfig, RuneCfg};
use rune::Tx;
use rune::prefs::Prefs;

use super::{Desk, NoticeKind};
use crate::grid;
use crate::theme::{self, Theme};

/// The interface sizes ⌘⌥+ and ⌘⌥− step through, in percent.
pub const INTERFACE_SIZES: [u16; 8] = [80, 90, 100, 110, 125, 150, 175, 200];

/// A saved or configured interface size, held to 80–200%; 100% unset.
pub fn interface_size(size: Option<u16>) -> u16 {
    size.map_or(100, |s| s.clamp(INTERFACE_SIZES[0], INTERFACE_SIZES[7]))
}

mod bars;
mod labs;
mod levels;
pub mod pair;

/// Settings destinations, separate from preferences and connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Appearance,
    Terminal,
    Notifications,
    RemoteHosts,
    Developer,
    /// Feature flags; listed only in developer mode, so not in [`Self::ALL`].
    Labs,
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
            Self::RemoteHosts => "Remote hosts",
            Self::Developer => "Developer",
            Self::Labs => "Labs",
            Self::About => "About & updates",
        }
    }

    pub const fn key(self) -> &'static str {
        match self {
            Self::Appearance => "settings-appearance",
            Self::Terminal => "settings-terminal",
            Self::Notifications => "settings-notifications",
            Self::RemoteHosts => "settings-hosts",
            Self::Developer => "settings-developer",
            Self::Labs => "settings-labs",
            Self::About => "settings-about",
        }
    }

    /// The phone's scroller for this category's level: each level keeps
    /// its own place, apart from the list's.
    pub const fn scroller(self) -> &'static str {
        match self {
            Self::Appearance => "settings-content-appearance",
            Self::Terminal => "settings-content-terminal",
            Self::Notifications => "settings-content-notifications",
            Self::RemoteHosts => "settings-content-hosts",
            Self::Developer => "settings-content-developer",
            Self::Labs => "settings-content-labs",
            Self::About => "settings-content-about",
        }
    }

    /// The destinations Settings lists: [`Self::ALL`], with Labs after
    /// Developer while developer mode is on.
    pub fn shown(developer_mode: bool) -> impl Iterator<Item = Self> {
        Self::ALL.into_iter().flat_map(move |category| {
            let labs = (developer_mode && category == Self::Developer).then_some(Self::Labs);
            std::iter::once(category).chain(labs)
        })
    }
}

#[derive(Default)]
pub struct SettingsUi {
    pub category: Option<Category>,
    pub query: rune::TextEdit,
    /// The existing theme palette returns here on both accept and cancel.
    pub return_from_themes: bool,
    /// Pair a phone: the code on screen and the devices that can connect.
    pub pairing: pair::Pairing,
    /// How far down a phone's category list is, as it reports.
    pub list_scroll: crate::desk::phone::navigation::Offset,
    /// Where the list goes back to once it is laid out again: where a
    /// level's back left it.
    restore_list: Option<f32>,
    /// When the page last closed, on the app clock.
    closed_at: Option<f64>,
}

/// What the person chose, as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub theme: Option<&'static Theme>,
    pub notifications: bool,
    pub option_as_meta: bool,
    pub copy_on_select: bool,
    /// The wheel over a full-screen program sends arrows (DEC 1007 permitting).
    pub wheel_arrows: bool,
    /// Ask before closing a terminal or tab with something running.
    pub confirm_close: bool,
    /// How much Liquid Glass shows through the window chrome, 0 to 100.
    pub glass: u8,
    /// The title bar: shown, revealed at the top edge, or gone.
    pub title_bar: TitleBar,
    /// Where the terminal tabs go.
    pub tab_bar: TabBar,
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
            wheel_arrows: prefs.get("wheel_arrows").unwrap_or(true),
            confirm_close: prefs.get("confirm_close").unwrap_or(false),
            glass: prefs.get::<u8>("glass").unwrap_or(0).min(100),
            title_bar: bars::saved(prefs),
            tab_bar: TabBar::Auto,
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
            wheel_arrows: file.wheel_arrows.unwrap_or(self.wheel_arrows),
            confirm_close: file.confirm_close.unwrap_or(self.confirm_close),
            glass: self.glass,
            title_bar: file.title_bar.unwrap_or(self.title_bar),
            tab_bar: file.tab_bar.unwrap_or(self.tab_bar),
        }
    }
}

/// `config.toml` as a client reads it, and what was wrong with the file if
/// it did not load; its `[[sessions]]` colours and `[[icons]]` are learned on
/// the way. Tests
/// never read the real file.
pub(super) fn load_file() -> (ClientConfig, Option<String>) {
    if cfg!(test) {
        return (ClientConfig::default(), None);
    }
    match phux_config_core::loader::load_client() {
        Ok(mut config) => {
            crate::session_color::learn(std::mem::take(&mut config.sessions));
            phux_config_core::pane_icon::learn(std::mem::take(&mut config.icons));
            if let Some(socket) = config.rune.ssh_agent.as_deref().map(str::trim) {
                let home = std::env::var_os("HOME").unwrap_or_default();
                let socket = socket.strip_prefix("~/").map_or_else(
                    || socket.into(),
                    |rest| std::path::Path::new(&home).join(rest),
                );
                phux_client_runtime::connection::set_ssh_agent(socket);
            }
            (config, None)
        }
        Err(err) => (ClientConfig::default(), Some(err.to_string())),
    }
}

impl Desk {
    /// True, with a word on why, when `config.toml` owns a setting.
    pub(super) fn file_managed(&mut self, set: bool) -> bool {
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
                interface_size: Some(self.interface_size),
                font_family: Some(self.font_family.clone()),
                notifications: Some(settings.notifications),
                option_as_meta: Some(settings.option_as_meta),
                copy_on_select: Some(settings.copy_on_select),
                wheel_arrows: Some(settings.wheel_arrows),
                confirm_close: Some(settings.confirm_close),
                tab_bar: Some(settings.tab_bar),
                title_bar: Some(settings.title_bar),
                ..RuneCfg::default()
            }
            .to_toml(),
        );
        self.say(NoticeKind::Info, "Copied settings as a [rune] table.");
    }

    /// Set the interface size; `None` is 100%. Like `zoom`, a file's
    /// `interface-size` is only where a window starts.
    pub fn set_interface_size(&mut self, size: Option<u16>) {
        self.interface_size = interface_size(size);
        let _ = self.prefs.set("interface_size", &self.interface_size);
    }

    /// Move `steps` along `INTERFACE_SIZES` from the current size.
    pub fn step_interface_size(&mut self, steps: i32) {
        let mut size = self.interface_size;
        for _ in 0..steps.unsigned_abs() {
            let next = if steps > 0 {
                INTERFACE_SIZES.iter().find(|&&s| s > size)
            } else {
                INTERFACE_SIZES.iter().rev().find(|&&s| s < size)
            };
            size = next.copied().unwrap_or(size);
        }
        self.set_interface_size(Some(size));
    }

    // A file's `font-size` is where each window starts; zooming still works.
    pub fn zoom(&mut self, size: Option<f32>) {
        self.font_size = size.map_or(grid::DEFAULT_FONT_SIZE, |s| {
            s.clamp(grid::MIN_FONT_SIZE, grid::MAX_FONT_SIZE)
        });
        let _ = self.prefs.set("font_size", &self.font_size);
        self.connection.fitted.clear();
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
        self.window_w > 0.0 && self.window_w < phux_design::tokens::SPACE.sidebar_w + MIN_STAGE
    }

    /// Whether the sidebar is drawn.
    /// Where the tabs go now: `auto` resolves to the title bar while the
    /// sidebar is not docked beside the panes, and to nowhere else while it
    /// is, since its rows are the tabs then. A peeking sidebar covers the
    /// panes for a moment and moves nothing.
    pub fn tab_bar(&self) -> TabBar {
        match self.settings.tab_bar {
            TabBar::Auto if self.sidebar_open && !self.sidebar_narrow() => TabBar::Hidden,
            TabBar::Auto => TabBar::Top,
            at => at,
        }
    }

    pub fn sidebar_shown(&self) -> bool {
        if self.sidebar_narrow() {
            self.sidebar_peek
        } else {
            self.sidebar_open
        }
    }

    /// The window is one surface: a desk with its sidebar away and one pane
    /// on screen. The pane loses its card and runs edge to edge, the
    /// window's controls sitting on its ground, as a terminal window's do.
    pub fn flush(&self) -> bool {
        self.phone.is_none()
            && !self.sidebar_shown()
            && matches!(
                self.connection.layout,
                Some(phux_client_core::layout::LayoutNode::Leaf(_))
            )
    }

    /// The root's layout: learn the window's size, and post a frame when it
    /// crosses the narrow line or starts the size glance, since on_layout
    /// runs after poll.
    pub fn window_laid_out(&mut self, [width, height]: [f32; 2]) {
        let was = self.sidebar_narrow();
        let resized = self.window_w > 0.0 && [width, height] != [self.window_w, self.window_h];
        self.window_w = width;
        self.window_h = height;
        let crossed = was != self.sidebar_narrow();
        if crossed {
            self.sidebar_peek = false;
        }
        if resized {
            self.glance_resize();
        }
        if (crossed || resized)
            && let Some(waker) = &self.waker
        {
            waker();
        }
    }

    pub fn settings_themes(&mut self) {
        self.open_themes();
        self.settings_ui.return_from_themes = true;
    }

    /// Install the chosen palette on the runtime. Live panes repaint with it.
    pub(super) fn apply_theme(&mut self) {
        self.install_terminal_theme();
        self.generation += 1;
    }

    fn install_terminal_theme(&self) {
        let theme = self.theme();
        self.worn.set(theme);
        if let Some(client) = &self.connection.client {
            let dark = self.system_dark.get().unwrap_or(true);
            client.set_terminal_theme(
                theme.map_or_else(|| Theme::system_terminal(dark), |t| Some(t.terminal())),
            );
        }
    }

    /// The theme this window wears: the viewed machine's own, a satellite's
    /// while its terminal shows, else the one chosen.
    pub fn theme(&self) -> Option<&'static Theme> {
        let shown = self.connection.shown.as_ref();
        let satellite = shown.and_then(|s| s.resource.host());
        let host = satellite.map_or(self.machines.label(), |h| h.as_str());
        crate::host_color::theme(host).or(self.settings.theme)
    }

    /// The window moved to or from a machine with its own theme: wear it.
    pub(super) fn follow_host_theme(&mut self) -> bool {
        if self.worn.get() == self.theme() {
            return false;
        }
        self.apply_theme();
        true
    }

    /// The view saw the system appearance: with no theme picked, the
    /// terminal follows a change to it.
    pub fn follow_appearance(&self, dark: bool) {
        if self.system_dark.replace(Some(dark)) != Some(dark) && self.theme().is_none() {
            self.install_terminal_theme();
        }
    }

    /// Omarchy changed its theme: a window following it repaints.
    /// Omarchy's theme and Pair a phone's answers. True when either changed
    /// what is on screen.
    pub(super) fn pump_settings(&mut self, tx: &mut Tx) -> bool {
        self.follow_omarchy() | self.pump_pairing(tx)
    }

    fn follow_omarchy(&mut self) -> bool {
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

    pub fn toggle_wheel_arrows(&mut self) {
        if self.file_managed(self.file.wheel_arrows.is_some()) {
            return;
        }
        self.settings.wheel_arrows = !self.settings.wheel_arrows;
        let _ = self.prefs.set("wheel_arrows", &self.settings.wheel_arrows);
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

    /// Selection finished: hand it to the clipboard when asked to.
    pub fn selection_done(&mut self, tx: &mut Tx) {
        if self.settings.copy_on_select {
            self.copy(tx);
        }
    }
}

mod search;

#[cfg(test)]
mod tests;
