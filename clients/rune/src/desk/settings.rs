//! The settings window's state: what is remembered between launches and how
//! changing it reaches the runtime.

use rune::prefs::Prefs;
use rune::{Key, KeyEvent, Tx};

use super::Desk;
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

    /// Unhandled arrows never mutate preferences. Escape backs out of the
    /// phone category before closing the sheet.
    pub(super) fn settings_key(&mut self, event: &KeyEvent, tx: &mut Tx) {
        if self.settings_search_focused() && tx.focused().is_none() {
            self.settings_soft(event, tx);
        } else if event.key == Key::Escape {
            self.settings_dismiss();
        }
    }

    /// Selection finished: hand it to the clipboard when asked to.
    pub fn selection_done(&mut self, tx: &mut Tx) {
        if self.settings.copy_on_select {
            self.copy(tx);
        }
    }
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
        desk.settings_open = true;
        desk
    }
}

#[cfg(test)]
mod tests {
    use rune::accesskit::{Action, ActionData};
    use rune::prefs::Prefs;
    use rune::{Env, Runtime, TouchPhase};
    use serde_json::{Value, json};

    use crate::desk::SettingsCategory as Category;
    use crate::desk::{Desk, Notice, NoticeKind};
    use crate::grid::{MAX_FONT_SIZE, MIN_FONT_SIZE};
    use crate::launch::Resolved;

    /// The first node at or under `node` with `field` equal to `want`.
    fn find_by<'a>(node: &'a Value, field: &str, want: &str) -> &'a Value {
        fn walk<'a>(node: &'a Value, field: &str, want: &str) -> Option<&'a Value> {
            if node[field] == want {
                return Some(node);
            }
            node["children"]
                .as_array()?
                .iter()
                .find_map(|child| walk(child, field, want))
        }
        walk(node, field, want)
            .unwrap_or_else(|| panic!("no {field} {want} in the accessibility tree"))
    }

    fn find<'a>(node: &'a Value, key: &str) -> &'a Value {
        find_by(node, "key", key)
    }

    fn runtime(desk: Desk) -> Runtime<Desk> {
        let mut runtime = Runtime::new(desk, crate::ui::view, (1100.0, 720.0), Env::new(true, 2.0));
        runtime.on_key = Some(Desk::key);
        runtime.frame(0.0);
        runtime
    }

    fn checked(on: bool) -> &'static str {
        if on { "checked" } else { "unchecked" }
    }

    mod navigation;

    #[test]
    fn a_touch_taps_a_setting_once_and_leaves_no_hover() {
        let mut desk = Desk::new(&Resolved::offline("touch"));
        desk.prefs = Prefs::memory();
        desk.settings_open = true;
        desk.settings_category(Category::Terminal);
        let mut runtime = runtime(desk);
        runtime.update(|_, tx| {
            tx.instant();
            tx.scroll_into_view("set-copy-select");
        });
        runtime.frame(0.0);
        let before = runtime.state.settings.copy_on_select;
        let (x, y) = runtime.find("set-copy-select").unwrap();
        runtime.touch(1, TouchPhase::Started, x, y, 0.0);
        runtime.frame(1.0 / 60.0);
        assert_eq!(runtime.state.settings.copy_on_select, before);
        runtime.touch(1, TouchPhase::Ended, x, y, 0.05);
        runtime.frame(1.0 / 60.0);
        assert_eq!(runtime.state.settings.copy_on_select, !before);
        assert_eq!(runtime.pointer(), None);
        assert!(!runtime.touching());
    }

    #[test]
    fn a_cancelled_touch_and_a_second_finger_change_no_settings() {
        let mut desk = Desk::new(&Resolved::offline("touch"));
        desk.prefs = Prefs::memory();
        desk.settings_open = true;
        desk.settings_category(Category::Terminal);
        let mut runtime = runtime(desk);
        runtime.update(|_, tx| {
            tx.instant();
            tx.scroll_into_view("set-copy-select");
        });
        runtime.frame(0.0);
        let before = runtime.state.settings.copy_on_select;
        let (x, y) = runtime.find("set-copy-select").unwrap();
        runtime.touch(1, TouchPhase::Started, x, y, 0.0);
        runtime.frame(1.0 / 60.0);
        runtime.touch(2, TouchPhase::Started, x, y, 0.01);
        runtime.touch(2, TouchPhase::Ended, x, y, 0.02);
        runtime.frame(1.0 / 60.0);
        assert_eq!(runtime.state.settings.copy_on_select, before);
        assert!(runtime.touching(), "the first finger is still down");
        runtime.touch(1, TouchPhase::Cancelled, x, y, 0.03);
        runtime.frame(1.0 / 60.0);
        assert_eq!(runtime.state.settings.copy_on_select, before);
        assert!(!runtime.touching());
    }

    #[test]
    fn a_touch_pans_settings_without_changing_a_preference() {
        let mut desk = Desk::new(&Resolved::offline("touch"));
        desk.prefs = Prefs::memory();
        desk.settings_open = true;
        desk.settings_category(Category::Terminal);
        desk.settings.theme = None;
        let before = (desk.settings, desk.font_size);
        let mut runtime = runtime(desk);
        runtime.set_viewport((800.0, 360.0), 2.0);
        runtime.frame(0.0);
        let elements = runtime.elements();
        let viewport = elements
            .iter()
            .position(|e| e.key.as_deref() == Some("settings-content"))
            .unwrap();
        let body = elements
            .iter()
            .find(|e| e.parent == Some(viewport))
            .unwrap();
        // Linux omits Option as Meta, so the shorter page can reach its end
        // before a 100-point finger movement. Assert the exact clamped pan.
        let expected = 100.0_f32.min((body.rect[3] - elements[viewport].rect[3]).max(0.0));
        assert!(expected > 0.0, "the fixture must have scrollable content");
        let (x, y) = runtime.find("text-size").unwrap();
        runtime.touch(1, TouchPhase::Started, x, y, 0.0);
        runtime.frame(1.0 / 60.0);
        runtime.touch(1, TouchPhase::Moved, x, y - 100.0, 0.05);
        runtime.frame(1.0 / 60.0);
        // Hold before lifting, so this checks the pan without a fling.
        runtime.touch(1, TouchPhase::Ended, x, y - 100.0, 0.3);
        runtime.frame(1.0 / 60.0);
        let (_, after) = runtime.find("text-size").unwrap();
        assert!(
            ((y - after) - expected).abs() < 1.0,
            "the sheet followed the finger to its content boundary: {y} → {after}, expected {expected}"
        );
        assert_eq!((runtime.state.settings, runtime.state.font_size), before);
        assert!(!runtime.touching());
    }

    #[test]
    fn settings_read_and_change_through_a_screen_reader() {
        let mut desk = Desk::new(&Resolved::offline("a11y"));
        // What the actions change stays in the test.
        desk.prefs = Prefs::memory();
        desk.settings_open = true;
        desk.settings_category(Category::Terminal);
        let mut runtime = runtime(desk);
        let tree = runtime.accessibility_json();
        assert_eq!(find(&tree, "settings-scrim")["name"], "Settings");
        assert_eq!(find(&tree, "settings-close")["name"], "Close");

        let on = runtime.state.settings.copy_on_select;
        let switch = find(&tree, "set-copy-select");
        assert_eq!(switch["role"], "switch");
        assert_eq!(switch["name"], "Copy on select");
        assert_eq!(
            switch["description"],
            "Finishing a selection puts it on the clipboard."
        );
        assert!(
            switch["states"]
                .as_array()
                .unwrap()
                .contains(&json!(checked(on)))
        );
        // Its words and drawing are what the name and state already say.
        assert!(switch.get("children").is_none(), "{switch}");
        assert!(runtime.accessibility_action("set-copy-select", Action::Click, None));
        runtime.frame(1.0 / 60.0);
        assert_eq!(runtime.state.settings.copy_on_select, !on);
        let switch = find(&runtime.accessibility_json(), "set-copy-select").clone();
        assert!(
            switch["states"]
                .as_array()
                .unwrap()
                .contains(&json!(checked(!on)))
        );

        let size = runtime.state.font_size;
        let stepper = find(&tree, "text-size");
        assert_eq!(stepper["role"], "slider");
        assert_eq!(stepper["value"], format!("{size:.0} pt"));
        assert_eq!(
            stepper["range"],
            json!([size, MIN_FONT_SIZE, MAX_FONT_SIZE, 1.0])
        );
        assert_eq!(find(&tree, "text-bigger")["name"], "Larger text");
        let want = if size + 3.0 <= MAX_FONT_SIZE {
            size + 3.0
        } else {
            size - 3.0
        };
        let data = Some(ActionData::NumericValue(f64::from(want)));
        assert!(runtime.accessibility_action("text-size", Action::SetValue, data));
        runtime.frame(2.0 / 60.0);
        assert_eq!(runtime.state.font_size, want);
    }

    /// A tip's offer is a named button; pressing it runs the action and
    /// takes the toast down.
    #[test]
    fn an_offer_is_a_button_that_runs_its_action() {
        let mut desk = Desk::new(&Resolved::offline("a11y"));
        desk.offer("New here?", "Show me", Desk::show_essentials);
        let mut runtime = runtime(desk);
        let tree = runtime.accessibility_json();
        assert_eq!(find_by(&tree, "role", "status")["name"], "New here?");
        assert_eq!(find(&tree, "toast-offer")["name"], "Show me");
        assert!(runtime.accessibility_action("toast-offer", Action::Click, None));
        runtime.frame(1.0 / 60.0);
        assert!(runtime.state.palette_open);
        assert_eq!(runtime.state.palette_mode, crate::palette::Mode::Essentials);
        assert!(runtime.state.notice.is_none(), "the offer goes once taken");
    }

    /// The tour is offered once per install, not once per window or connection.
    #[test]
    fn the_tour_is_offered_once() {
        let mut desk = Desk::new(&Resolved::offline("tour"));
        desk.prefs = Prefs::memory();
        desk.offer_tour();
        let first = desk
            .notice
            .clone()
            .expect("a first connection offers the tour");
        assert_eq!(first.kind, NoticeKind::Tip);
        assert!(first.action.is_some());
        desk.notice = None;
        desk.offer_tour();
        assert!(
            desk.notice.is_none(),
            "an offer already made is not repeated"
        );
    }

    #[test]
    fn a_notice_is_announced_and_an_error_at_once() {
        for (kind, role) in [(NoticeKind::Info, "status"), (NoticeKind::Error, "alert")] {
            let mut desk = Desk::new(&Resolved::offline("a11y"));
            desk.notice = Some(Notice {
                kind,
                text: "Copied".into(),
                action: None,
                expires: f64::MAX,
            });
            let tree = runtime(desk).accessibility_json();
            let notice = find_by(&tree, "role", role);
            assert_eq!(notice["name"], "Copied", "{kind:?}");
            assert_eq!(find(&tree, "toast-close")["name"], "Dismiss");
        }
    }
}
