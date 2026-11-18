use rune::accesskit::{Action, ActionData};
use rune::prefs::Prefs;
use rune::{Env, Runtime};
use serde_json::{Value, json};

use crate::desk::SettingsCategory as Category;
use crate::desk::{Desk, NoticeKind};
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
    walk(node, field, want).unwrap_or_else(|| panic!("no {field} {want} in the accessibility tree"))
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
    desk.clear_notices();
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
        desk.say(kind, "Copied");
        let tree = runtime(desk).accessibility_json();
        let notice = find_by(&tree, "role", role);
        assert_eq!(notice["name"], "Copied", "{kind:?}");
        assert_eq!(find(&tree, "toast-close")["name"], "Dismiss");
    }
}

#[test]
fn glass_is_saved_like_the_theme() {
    let mut desk = crate::test_support::settings_window();
    desk.prefs = Prefs::memory();
    desk.set_glass(40);
    assert_eq!(super::Settings::load(&desk.prefs).glass, 40);
    desk.set_glass(250);
    assert_eq!(super::Settings::load(&desk.prefs).glass, 100, "clamped");
}
