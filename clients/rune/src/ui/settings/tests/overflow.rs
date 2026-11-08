//! Settings' overflow cues, through the real view and renderer instances.

use super::{Category, Desk, Runtime, rect, runtime};
use rune::accesskit::Action;

fn settle(rt: &mut Runtime<Desk>) {
    rt.frame(1.0);
    rt.frame(1.0);
}

fn fades(rt: &Runtime<Desk>, viewport: &str) -> Vec<[f32; 4]> {
    let [_, y, _, h] = rect(rt, viewport);
    rt.instances
        .iter()
        .filter(|i| (i.clip[1] - y).abs() < 0.5 && (i.clip[3] - y - h).abs() < 0.5)
        .map(|i| rune::render::unpack_fade(i.fade).0)
        .collect()
}

fn assert_edges(rt: &Runtime<Desk>, viewport: &str, top: f32, bottom: f32) {
    let masks = fades(rt, viewport);
    assert!(!masks.is_empty(), "{viewport} has drawn content");
    assert!(
        masks
            .iter()
            .any(|f| (f[1] - top).abs() < 0.5 && (f[3] - bottom).abs() < 0.5),
        "{viewport} expected top={top}, bottom={bottom}: {masks:?}"
    );
}

fn wheel(rt: &mut Runtime<Desk>, viewport: &str, delta: f32) {
    let [x, y, w, h] = rect(rt, viewport);
    rt.wheel(x + w / 2.0, y + h / 2.0, delta);
    settle(rt);
}

#[test]
fn overflowing_settings_fade_only_the_hidden_edges_in_both_forms() {
    for (phone, size) in [(false, (640.0, 360.0)), (true, (393.0, 852.0))] {
        let mut rt = runtime(size, phone);
        rt.update(|d, tx| {
            tx.instant();
            // Real search results spanning multiple categories, not filler rows.
            d.settings_ui.query = rune::TextEdit::new("a");
        });
        settle(&mut rt);
        assert_edges(&rt, "settings-content", 0.0, -24.0);
        wheel(&mut rt, "settings-content", -60.0);
        assert_edges(&rt, "settings-content", 16.0, -24.0);
        wheel(&mut rt, "settings-content", -100_000.0);
        assert_edges(&rt, "settings-content", 16.0, 0.0);
        wheel(&mut rt, "settings-content", 100_000.0);
        assert_edges(&rt, "settings-content", 0.0, -24.0);
    }
}

#[test]
fn short_settings_and_phone_categories_have_no_fade() {
    for (phone, size) in [(false, (1100.0, 720.0)), (true, (393.0, 852.0))] {
        let mut rt = runtime(size, phone);
        rt.update(|d, tx| {
            tx.instant();
            d.settings_category(Category::Developer);
        });
        settle(&mut rt);
        assert_edges(&rt, "settings-content", 0.0, 0.0);
        assert!(
            fades(&rt, "settings-content")
                .iter()
                .all(|f| f[1] == 0.0 && f[3] == 0.0)
        );
    }
}

#[test]
fn keyboard_reveal_keeps_settings_controls_clear_of_the_fades() {
    let targets = ["copy-diagnostics", "set-copy-select", "settings-theme"];
    for (phone, size) in [(false, (640.0, 360.0)), (true, (320.0, 568.0))] {
        let mut rt = runtime(size, phone);
        rt.update(|d, tx| {
            tx.instant();
            d.settings_ui.query = rune::TextEdit::new("a");
        });
        settle(&mut rt);
        assert!(rt.accessibility_action("settings-query", Action::Focus, None));
        let mut visited = std::collections::HashSet::new();
        for _ in 0..30 {
            rt.key(rune::KeyEvent {
                key: rune::Key::Tab,
                mods: rune::Mods::default(),
            });
            settle(&mut rt);
            let focused = rt
                .elements()
                .into_iter()
                .find(|e| e.focused)
                .expect("Tab focus");
            let Some(key) = focused.key.filter(|key| targets.contains(&key.as_str())) else {
                continue;
            };
            visited.insert(key.clone());
            let [_, y, _, h] = rect(&rt, &key);
            let [_, top, _, height] = rect(&rt, "settings-content");
            assert!(
                y >= top && y + h <= top + height + 0.5,
                "{key} phone={phone} fully visible: y={y} h={h}, viewport={top}+{height}"
            );
            // At an end, the matching mask is hard. Else the control's
            // whole row, not just its text, stops outside the fade.
            for fade in fades(&rt, "settings-content") {
                assert!(fade[1] == 0.0 || y >= top + 16.0 - 0.5, "{key} clear above");
                assert!(
                    fade[3] == 0.0 || y + h <= top + height - 24.0 + 0.5,
                    "{key} clear below"
                );
            }
        }
        assert!(
            targets.iter().all(|key| visited.contains(*key)),
            "phone={phone}: {visited:?}"
        );
    }
}

#[test]
fn minimum_desktop_category_rail_has_directional_overflow_cues() {
    let mut rt = runtime((640.0, 360.0), false);
    settle(&mut rt);
    assert_edges(&rt, "settings-navigation", 0.0, -24.0);
    wheel(&mut rt, "settings-navigation", -40.0);
    assert_edges(&rt, "settings-navigation", 16.0, -24.0);
    wheel(&mut rt, "settings-navigation", -100_000.0);
    assert_edges(&rt, "settings-navigation", 16.0, 0.0);
}
