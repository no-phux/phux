//! The app-owned editor survives Rune's range-based history and buffer reuse.
use super::*;
use rune::accesskit::{ActionData, TextPosition, TextSelection};

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

#[test]
fn a_search_past_64_kib_keeps_accessible_selection_and_replacement() {
    for (phone, size) in [(false, (1100.0, 720.0)), (true, (393.0, 852.0))] {
        let mut rt = runtime(size, phone);
        let before = rt.state.settings;
        assert!(rt.accessibility_action("settings-query", Action::Focus, None));
        rt.frame(0.0);
        let pasted = "font 🦀 日本語 e\u{301} ".repeat(3072);
        assert!(pasted.len() > 64 * 1024);
        rt.paste(&pasted);
        rt.frame(0.0);
        let tree = rt.accessibility_tree();
        let (id, field) = tree
            .nodes
            .iter()
            .find(|(_, node)| node.author_id() == Some("settings-query"))
            .unwrap();
        assert_eq!(field.value(), Some(pasted.as_str()));
        assert!(field.children().is_empty(), "long fields use lazy ranges");
        let selection = field.text_selection().unwrap();
        assert_eq!(selection.anchor.node, *id);
        assert_eq!(selection.focus.node, *id);
        assert_eq!(selection.focus.character_index, pasted.len());

        // The adapters address the field itself in UTF-8 bytes. A stale end
        // and a split scalar must clamp, not panic or edit another control.
        let start = pasted.find('🦀').unwrap();
        let position = |character_index| TextPosition {
            node: *id,
            character_index,
        };
        let selection = TextSelection {
            anchor: position(usize::MAX),
            focus: position(start + 1),
        };
        assert!(rt.accessibility_action(
            "settings-query",
            Action::SetTextSelection,
            Some(ActionData::SetTextSelection(selection)),
        ));
        rt.frame(0.0);
        let edit = &rt.state.settings_ui.query;
        assert_eq!((edit.anchor, edit.focus), (pasted.len(), start));
        assert!(rt.accessibility_action(
            "settings-query",
            Action::ReplaceSelectedText,
            Some(ActionData::Value("size".into())),
        ));
        rt.frame(0.0);
        assert_eq!(rt.state.settings_ui.query.text, "font size");
        let tree = rt.accessibility_tree();
        let (id, field) = tree
            .nodes
            .iter()
            .find(|(_, node)| node.author_id() == Some("settings-query"))
            .unwrap();
        assert!(
            !field.children().is_empty(),
            "short fields regain text runs"
        );
        assert_ne!(field.text_selection().unwrap().focus.node, *id);
        assert!(rt.state.settings_search_focused());
        assert_eq!(rt.state.settings, before);
    }
}
