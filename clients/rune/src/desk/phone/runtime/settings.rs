//! Settings owns its search keyboard, never the terminal behind the page.

use super::*;
use crate::keys::Shortcut;

fn open(hidden: bool) -> (std::sync::Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = fixture();
    rt.update(|d, _| {
        d.phone.as_mut().unwrap().keyboard = !hidden;
        d.settings_open = true;
    });
    settle(&mut rt);
    (client, rt)
}

#[test]
fn settings_search_shows_its_keyboard_without_changing_terminal_choice() {
    let (client, mut rt) = open(true);
    assert!(rt.find("phone-top").is_none());
    assert!(rt.find("phone-tabs").is_none());
    assert!(rt.find("phone-cap-116").is_none());
    tap(&mut rt, "settings-query", 0.1);
    assert!(rt.state.settings_search_focused());
    tap(&mut rt, "phone-cap-shift", 0.3);
    tap(&mut rt, "phone-cap-116", 0.5);
    tap(&mut rt, "phone-cap-104", 0.7);
    assert_eq!(rt.state.settings_ui.query.text, "Th");
    assert!(!rt.state.phone.as_ref().unwrap().capital);
    tap(&mut rt, "phone-keyboard-toggle", 0.9);
    assert!(!rt.state.settings_search_focused());
    assert_eq!(rt.state.settings_ui.query.text, "Th");
    assert!(rt.find("phone-cap-116").is_none());
    assert!(!rt.state.phone.as_ref().unwrap().keyboard);
    rt.state.settings_open = false;
    settle(&mut rt);
    assert!(rt.find("phone-cap-116").is_none());
    no_terminal_input(&client);
}

#[test]
fn clear_keeps_search_keyboard_and_done_blurs_without_opening_palette() {
    let (client, mut rt) = open(false);
    tap(&mut rt, "settings-query", 0.1);
    tap(&mut rt, "phone-cap-116", 0.3);
    tap(&mut rt, "settings-clear", 0.5);
    assert!(rt.state.settings_search_focused());
    assert!(rt.state.settings_ui.query.text.is_empty());
    tap(&mut rt, "phone-cap-104", 0.7);
    tap(&mut rt, "phone-command", 0.9);
    assert!(!rt.state.settings_search_focused());
    assert_eq!(rt.state.settings_ui.query.text, "h");
    assert!(!rt.state.palette_open);
    assert!(rt.state.phone.as_ref().unwrap().keyboard);
    rt.update(|d, tx| {
        d.soft(Soft::Char('x'), tx);
        d.phone_commands(tx);
    });
    settle(&mut rt);
    assert_eq!(rt.state.settings_ui.query.text, "h");
    assert!(!rt.state.palette_open);
    no_terminal_input(&client);
}

#[test]
fn native_find_focuses_settings_search_instead_of_the_terminal() {
    for phone in [false, true] {
        let (client, mut rt) = open(true);
        if !phone {
            rt.state.phone = None;
            rt.set_viewport((1100.0, 720.0), 2.0);
        }
        settle(&mut rt);
        tap(&mut rt, "settings-terminal", 0.1);
        rt.key(KeyEvent {
            key: Key::Command('f'),
            mods: Mods {
                cmd: cfg!(target_os = "macos"),
                ctrl: !cfg!(target_os = "macos"),
                shift: !cfg!(target_os = "macos"),
                ..Mods::default()
            },
        });
        settle(&mut rt);
        assert!(rt.state.settings_search_focused());
        assert!(rt.state.connection.find.is_none());
        rt.update(|d, tx| d.blur_settings_search(tx));
        settle(&mut rt);
        rt.update(|d, tx| d.modal_command(Modal::Settings, Shortcut::Find, tx));
        settle(&mut rt);
        assert!(
            rt.state.settings_search_focused(),
            "menu Find shares chord policy"
        );
        assert!(rt.state.connection.find.is_none());
        no_terminal_input(&client);
    }
}

#[test]
fn accessory_commands_cannot_open_a_hidden_palette_behind_the_pane_picker() {
    let (client, mut rt) = fixture();
    rt.state.phone.as_mut().unwrap().picker = true;
    settle(&mut rt);
    tap(&mut rt, "phone-command", 0.1);
    assert!(rt.state.phone.as_ref().unwrap().picker);
    assert!(!rt.state.palette_open);
    tap(&mut rt, "phone-cap-116", 0.3);
    tap(&mut rt, "phone-key-Tab", 0.5);
    assert!(rt.state.phone.as_ref().unwrap().picker);
    assert!(!rt.state.palette_open);
    assert!(rt.state.palette_query.text.is_empty());
    no_terminal_input(&client);
}

#[test]
fn settings_soft_edit_cannot_outlive_page_or_reach_a_terminal() {
    let (client, mut rt) = open(true);
    tap(&mut rt, "settings-query", 0.1);
    rt.update(|d, tx| {
        d.soft(Soft::Char('x'), tx);
        d.settings_open = false;
    });
    settle(&mut rt);
    assert_eq!(rt.state.settings_ui.query.text, "x");
    no_terminal_input(&client);
}

#[test]
fn phone_search_stays_above_its_keyboard_at_both_supported_sizes() {
    for size in [(393.0, 852.0), (320.0, 568.0)] {
        let (client, mut rt) = open(true);
        rt.set_viewport(size, 2.0);
        settle(&mut rt);
        tap(&mut rt, "settings-query", 0.1);
        let elements = rt.elements();
        let bounds = |key: &str| {
            elements
                .iter()
                .find(|e| e.key.as_deref() == Some(key))
                .unwrap()
                .rect
        };
        let query = bounds("settings-query");
        let accessory = bounds("phone-accessory");
        let keyboard = bounds("phone-keyboard");
        assert!(query[3] >= 44.0, "search keeps its touch target");
        assert!(query[1] + query[3] <= accessory[1]);
        assert!(accessory[1] + accessory[3] <= keyboard[1] + 0.5);
        rt.paste("font");
        settle(&mut rt);
        assert_eq!(rt.state.settings_ui.query.text, "font");
        assert!(
            rt.find("text-size").is_some(),
            "native paste searches real controls"
        );
        tap(&mut rt, "phone-cap-return", 0.3);
        assert!(!rt.state.settings_search_focused());
        assert_eq!(rt.state.settings_ui.query.text, "font");
        assert!(rt.find("phone-keyboard").is_none());
        no_terminal_input(&client);
    }
}

#[test]
fn native_enter_escape_and_close_reconcile_settings_focus() {
    for phone in [false, true] {
        let (client, mut rt) = open(true);
        if !phone {
            rt.state.phone = None;
            rt.set_viewport((1100.0, 720.0), 2.0);
        }
        settle(&mut rt);
        tap(&mut rt, "settings-query", 0.1);
        rt.paste("clipboard");
        rt.key(crate::keys::plain(Key::Enter));
        settle(&mut rt);
        assert!(!rt.state.settings_search_focused());
        assert_eq!(rt.state.settings_ui.query.text, "clipboard");
        assert!(
            !rt.elements()
                .iter()
                .any(|e| e.key.as_deref() == Some("settings-query") && e.focused)
        );
        tap(&mut rt, "settings-query", 0.3);
        rt.key(crate::keys::plain(Key::Escape));
        settle(&mut rt);
        assert!(rt.state.settings_open);
        assert!(rt.state.settings_search_focused());
        assert!(rt.state.settings_ui.query.text.is_empty());
        rt.update(|d, tx| d.modal_command(Modal::Settings, Shortcut::Settings, tx));
        settle(&mut rt);
        assert!(!rt.state.settings_open);
        assert!(!rt.state.settings_ui.query.focused);
        assert!(rt.find("settings-query").is_none());
        assert!(
            !rt.elements()
                .iter()
                .any(|e| e.focused && e.key.as_deref().is_some_and(|k| k.starts_with("settings-")))
        );
        rt.update(|d, _| d.settings_open = true);
        settle(&mut rt);
        assert!(
            rt.find("settings-terminal").is_some(),
            "reopened phone={phone}, category={:?}, query={:?}, focused={}",
            rt.state.settings_ui.category,
            rt.state.settings_ui.query.text,
            rt.state.settings_ui.query.focused
        );
        tap(&mut rt, "settings-terminal", 0.5);
        rt.key(crate::keys::plain(Key::Escape));
        settle(&mut rt);
        assert_eq!(
            rt.state.settings_open, phone,
            "phone backs out before closing"
        );
        assert_eq!(
            rt.state.settings_ui.category,
            if phone {
                None
            } else {
                Some(crate::desk::SettingsCategory::Terminal)
            }
        );
        no_terminal_input(&client);
    }
}

#[test]
fn enlarged_theme_row_touch_and_keyboard_keep_preview_and_terminal_input_ownership() {
    for size in [(393.0, 480.0), (320.0, 340.0)] {
        let (client, mut rt) = open(true);
        rt.set_viewport(size, 2.0);
        rt.update(|d, tx| {
            d.native = Some(crate::mobile::Platform::test());
            d.set_accessibility(false, "UICTContentSizeCategoryAccessibilityXXXL");
            d.settings.theme = crate::theme::find("phux-light");
            d.settings_category(crate::desk::SettingsCategory::Appearance);
            tx.instant();
            tx.scroll_key_to("settings-theme", 24.0);
        });
        settle(&mut rt);
        let before = rt.state.settings.theme;
        let button = crate::ui::element(&rt, "settings-theme");
        let [x, y, w, h] = button.visible;
        assert!(w >= 44.0 && h >= 44.0);
        rt.touch(1, TouchPhase::Started, x + w / 2.0, y + h / 2.0, 0.1);
        rt.touch(1, TouchPhase::Ended, x + w / 2.0, y + h / 2.0, 0.15);
        settle(&mut rt);
        assert!(rt.state.palette_open);
        rt.paste("nord");
        settle(&mut rt);
        assert_eq!(rt.state.settings.theme, crate::theme::find("nord"));
        rt.key(crate::keys::plain(Key::Escape));
        settle(&mut rt);
        assert_eq!(rt.state.settings.theme, before);
        assert!(rt.state.settings_open && !rt.state.palette_open);
        assert!(crate::ui::element(&rt, "settings-theme").focused);
        assert!(!rt.state.phone.as_ref().unwrap().keyboard);
        no_terminal_input(&client);
        // Rune's native button contract uses Space, not Return (default button).
        rt.key(crate::keys::plain(Key::Text(" ".into())));
        settle(&mut rt);
        assert!(rt.state.palette_open);
        rt.paste("nord");
        settle(&mut rt);
        rt.key(crate::keys::plain(Key::Enter));
        settle(&mut rt);
        assert_eq!(rt.state.settings.theme, crate::theme::find("nord"));
        assert!(rt.state.settings_open && !rt.state.palette_open);
        assert!(crate::ui::element(&rt, "settings-theme").focused);
        assert!(!rt.state.settings_search_focused());
        no_terminal_input(&client);
    }
}

/// The native phone reaches Labs the desktop's way: Developer, then
/// Developer mode, then the Labs category and its flags. Each drill-in
/// finishes its slide before the next tap.
#[test]
fn a_native_phone_reaches_labs_through_developer_mode() {
    let (_client, mut rt) = open(true);
    rt.update(|d, tx| {
        d.native = Some(crate::mobile::Platform::test());
        tx.instant();
    });
    settle(&mut rt);
    let mut at = 0.0;
    let mut step = |rt: &mut Runtime<Desk>, key: &str| {
        tap(rt, key, at);
        for frame in 1..=60 {
            rt.frame(at + f64::from(frame) / 60.0);
        }
        at += 1.1;
    };
    assert!(
        rt.find("settings-labs").is_none(),
        "hidden until developer mode"
    );
    step(&mut rt, "settings-developer");
    step(&mut rt, "set-developer-mode");
    assert!(rt.state.developer_mode);
    step(&mut rt, "settings-back");
    step(&mut rt, "settings-labs");
    let echo = phux_config_core::labs::PREDICTIVE_ECHO;
    step(&mut rt, &format!("set-lab-{echo}"));
    assert_eq!(rt.state.labs.get(echo), Some(true));
}

/// Each Settings level keeps its own place: a category opens at its top
/// however far down the list was, and back brings the list home where it
/// was left, not where the category was scrolled.
#[test]
fn a_settings_level_opens_at_its_top_and_back_restores_the_list() {
    let (_client, mut rt) = open(true);
    rt.set_viewport((320.0, 568.0), 2.0);
    rt.update(|d, tx| {
        // Large text, so the category list scrolls on the small phone.
        d.set_accessibility(false, "UICTContentSizeCategoryAccessibilityXXXL");
        tx.instant();
    });
    settle(&mut rt);
    rt.update(|_, tx| tx.scroll_to_instant("settings-content", 0.0, 160.0));
    settle(&mut rt);
    let left = rt.state.settings_ui.list_scroll.get();
    assert!(left > 100.0, "the list scrolled: {left}");
    let row = rt.presented("settings-terminal").unwrap().0;
    let click = rune::accesskit::Action::Click;
    assert!(rt.accessibility_action("settings-terminal", click, None));
    for _ in 0..60 {
        rt.frame(1.0 / 60.0);
    }
    // The category's first control sits at the top of its own scroller.
    let scroller = rt.presented(rt.state.settings_scroller()).unwrap().0;
    let first = rt.presented("text-size").unwrap().0;
    assert!(
        first[1] >= scroller[1] && first[1] < scroller[1] + 160.0,
        "the category opened at its top: {first:?} in {scroller:?}"
    );
    rt.update(|d, tx| tx.scroll_to_instant(d.settings_scroller(), 0.0, 400.0));
    settle(&mut rt);
    assert!(rt.accessibility_action("settings-back", click, None));
    for _ in 0..60 {
        rt.frame(1.0 / 60.0);
    }
    assert_eq!(rt.state.settings_ui.list_scroll.get(), left);
    let back = rt.presented("settings-terminal").unwrap().0;
    assert!((back[1] - row[1]).abs() < 0.5, "{back:?} against {row:?}");
}
