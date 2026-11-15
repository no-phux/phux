use super::*;
use crate::keys::Shortcut;
use rune::{Env, Key, KeyEvent, Mods, Runtime};

fn runtime(phone: bool) -> Runtime<Desk> {
    let mut desk = crate::test_support::settings_window().with_phone(phone);
    desk.settings_open = false;
    let mut rt = Runtime::new(desk, crate::ui::view, (900.0, 700.0), Env::new(true, 1.0));
    rt.on_key = Some(Desk::key);
    rt
}

#[test]
fn settings_above_a_scene_owns_input_and_leaves_the_scene_on_dismissal() {
    for phone in [false, true] {
        let mut rt = runtime(phone);
        rt.update(|desk, tx| {
            desk.toggle_reef();
            desk.shortcut(Shortcut::Settings, tx);
        });
        rt.frame(0.0);
        assert_eq!(rt.state.modal(), Some(Modal::Settings));
        assert_eq!(rt.state.surfaces().next_back(), Some(Surface::Settings));
        assert!(rt.find("settings-close").is_some());
        rt.key(KeyEvent {
            key: Key::Escape,
            mods: Mods::default(),
        });
        rt.frame(0.1);
        assert!(!rt.state.settings_open);
        assert_eq!(rt.state.playground, Some(Playground::Reef));
        assert_eq!(rt.state.modal(), Some(Modal::Reef));
    }
}

#[test]
fn every_playground_transition_has_one_owner() {
    type Open = (Playground, fn(&mut Desk));
    let open: [Open; 6] = [
        (Playground::Galaxy, Desk::toggle_galaxy),
        (Playground::Reef, Desk::toggle_reef),
        (Playground::Pitch, Desk::toggle_pitch),
        (Playground::Kitchen, Desk::toggle_kitchen),
        (Playground::PitLane, Desk::toggle_pitlane),
        (Playground::Hive, Desk::toggle_hive),
    ];
    for (first, open_first) in open {
        for (second, open_second) in open {
            let mut rt = runtime(false);
            rt.update(|desk, _| {
                open_first(desk);
                open_second(desk);
            });
            let expected = (first != second).then_some(second);
            assert_eq!(rt.state.playground, expected);
            assert_eq!(
                rt.state
                    .surfaces()
                    .filter(|s| matches!(s, Surface::Playground(_)))
                    .count(),
                usize::from(expected.is_some())
            );
        }
    }
}

#[test]
fn phone_palette_and_navigation_use_the_same_foreground_order_as_painting() {
    let mut rt = runtime(true);
    rt.update(|desk, _| {
        desk.open_phone_sessions();
        desk.toggle_palette();
    });
    rt.frame(0.0);
    assert!(rt.find("phone-navigation-page").is_some());
    assert_eq!(rt.state.modal(), Some(Modal::Palette));
    rt.key(KeyEvent {
        key: Key::Escape,
        mods: Mods::default(),
    });
    rt.frame(0.1);
    assert!(!rt.state.palette_open);
    assert_eq!(rt.state.modal(), Some(Modal::PhoneNavigation));
}

#[test]
fn settings_theme_palette_returns_to_its_parent_on_both_forms() {
    for phone in [false, true] {
        let mut rt = runtime(phone);
        rt.update(|desk, _| {
            desk.toggle_settings();
            desk.settings_themes();
        });
        rt.frame(0.0);
        assert_eq!(rt.state.modal(), Some(Modal::Palette));
        rt.key(KeyEvent {
            key: Key::Escape,
            mods: Mods::default(),
        });
        rt.frame(0.1);
        assert!(rt.state.settings_open);
        assert_eq!(rt.state.modal(), Some(Modal::Settings));
    }
}
