//! The app-owned editor survives Rune's range-based history and buffer reuse.
use super::*;

fn shortcut(rt: &mut Runtime<Desk>, letter: char, shift: bool) {
    rt.key(KeyEvent {
        key: Key::Command(letter),
        mods: Mods {
            cmd: true,
            ctrl: !cfg!(target_os = "macos"),
            shift,
            ..Mods::default()
        },
    });
    rt.frame(0.0);
}

#[test]
fn long_unicode_search_paste_replacement_undo_and_redo_keep_the_editor() {
    for (phone, size) in [(false, (1100.0, 720.0)), (true, (393.0, 852.0))] {
        let mut rt = runtime(size, phone);
        let before = rt.state.settings;
        assert!(rt.accessibility_action("settings-query", Action::Focus, None));
        rt.frame(0.0);
        let pasted = "font 日本語 🦀 e\u{301}\r\n".repeat(256);
        let expected = pasted.replace(['\r', '\n'], "");
        rt.paste(&pasted);
        rt.frame(0.0);
        assert_eq!(rt.state.settings_ui.query.text, expected);
        shortcut(&mut rt, 'a', false);
        rt.paste("clipboard");
        rt.frame(0.0);
        assert_eq!(rt.state.settings_ui.query.text, "clipboard");
        assert!(rt.find("set-copy-select").is_some());
        shortcut(&mut rt, 'z', false);
        assert_eq!(rt.state.settings_ui.query.text, expected);
        shortcut(&mut rt, 'z', true);
        assert_eq!(rt.state.settings_ui.query.text, "clipboard");
        shortcut(&mut rt, 'z', false);
        shortcut(&mut rt, 'z', false);
        assert!(rt.state.settings_ui.query.text.is_empty());
        shortcut(&mut rt, 'z', true);
        assert_eq!(rt.state.settings_ui.query.text, expected);
        assert!(rt.state.settings_search_focused());
        assert_search_caret_centered(&rt);
        assert_eq!(rt.state.settings, before);
    }
}
