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

    /// Keys while settings is open and no control has them: arrows walk the
    /// themes, previewing live, as a focused theme grid does.
    pub(super) fn settings_key(&mut self, event: &KeyEvent, tx: &Tx) {
        if tx.focused().is_some() {
            return;
        }
        let themes: Vec<_> = theme::all().collect();
        let at = self
            .settings
            .theme
            .and_then(|t| themes.iter().position(|x| *x == t))
            .map_or(0, |i| i + 1);
        let count = themes.len() + 1;
        let step = match event.key {
            Key::Escape => return self.close_settings(),
            Key::Right | Key::Down => 1,
            Key::Left | Key::Up => count - 1,
            _ => return,
        };
        let next = (at + step) % count;
        self.choose_theme(next.checked_sub(1).map(|i| themes[i]));
    }

    /// Selection finished: hand it to the clipboard when asked to.
    pub fn selection_done(&mut self, tx: &mut Tx) {
        if self.settings.copy_on_select {
            self.copy(tx);
        }
    }
}

#[cfg(test)]
mod tests {
    use rune::accesskit::{Action, ActionData};
    use rune::prefs::Prefs;
    use rune::{Env, Runtime};
    use serde_json::{Value, json};

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
        runtime.frame(0.0);
        runtime
    }

    fn checked(on: bool) -> &'static str {
        if on { "checked" } else { "unchecked" }
    }

    #[test]
    fn settings_read_and_change_through_a_screen_reader() {
        let mut desk = Desk::new(&Resolved::offline("a11y"));
        // What the actions change stays in the test.
        desk.prefs = Prefs::memory();
        desk.settings_open = true;
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
