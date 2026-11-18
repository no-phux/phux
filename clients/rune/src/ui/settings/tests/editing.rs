//! The app-owned editor survives Rune's range-based history and buffer reuse.
use super::*;
use rune::accesskit::{ActionData, Node, NodeId, Role, TextPosition, TextSelection, TreeUpdate};

fn node(tree: &TreeUpdate, id: NodeId) -> &Node {
    &tree
        .nodes
        .iter()
        .find(|(candidate, _)| *candidate == id)
        .unwrap()
        .1
}

fn assert_cluster_geometry(node: &Node, value: &str) {
    let lengths = node.character_lengths();
    let positions = node.character_positions().expect("drawn cluster positions");
    let widths = node.character_widths().expect("drawn cluster widths");
    assert!(!lengths.is_empty());
    assert_eq!(positions.len(), lengths.len());
    assert_eq!(widths.len(), lengths.len());
    let mut byte = 0;
    for &length in lengths {
        byte += usize::from(length);
        assert!(
            value.is_char_boundary(byte),
            "geometry must not split a scalar"
        );
    }
    assert_eq!(byte, value.len());
    assert!(positions.iter().all(|value| value.is_finite()));
    assert!(
        widths
            .iter()
            .all(|value| value.is_finite() && *value >= 0.0)
    );
}

/// Large values keep field-addressed ranges; geometry is separate, not a
/// second copy of the value in ordinary TextRuns.
fn long_geometry(tree: &TreeUpdate, field: &Node, value: &str) -> NodeId {
    assert_eq!(field.children().len(), 1);
    let geometry_id = field.children()[0];
    let geometry = node(tree, geometry_id);
    assert_eq!(geometry.role(), Role::GenericContainer);
    assert_eq!(geometry.class_name(), Some("\x1frune-lines"));
    assert_eq!(geometry.value(), None);
    assert_eq!(geometry.children().len(), 1, "search is one visual line");
    let line = node(tree, geometry.children()[0]);
    assert_eq!(line.role(), Role::GenericContainer);
    assert_eq!(line.class_name(), Some("\x1frune-line"));
    assert_eq!(
        line.author_id(),
        Some(format!("0:{}", value.len()).as_str())
    );
    assert_eq!(line.value(), None, "the field owns the complete value");
    assert!(line.children().is_empty());
    assert_cluster_geometry(line, value);
    let bounds = line.bounds().expect("drawn visual-line bounds");
    assert!(
        [bounds.x0, bounds.y0, bounds.x1, bounds.y1]
            .into_iter()
            .all(f64::is_finite)
    );
    assert!(bounds.x1 > bounds.x0 && bounds.y1 > bounds.y0);
    let field_bounds = field.bounds().unwrap();
    assert!(bounds.y1 > field_bounds.y0 && bounds.y0 < field_bounds.y1);
    geometry_id
}

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
        let geometry_id = long_geometry(&tree, field, &pasted);
        assert_ne!(geometry_id, *id);
        assert_eq!(tree.focus, *id);
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
        assert_eq!(field.value(), Some("font size"));
        let mut shown = String::new();
        for &run_id in field.children() {
            let run = node(&tree, run_id);
            assert_eq!(run.role(), Role::TextRun);
            let value = run.value().unwrap();
            assert_cluster_geometry(run, value);
            shown.push_str(value);
        }
        assert_eq!(shown, "font size");
        let selection = field.text_selection().unwrap();
        assert!(field.children().contains(&selection.anchor.node));
        assert!(field.children().contains(&selection.focus.node));
        assert_eq!(selection.anchor, selection.focus);
        assert_eq!(tree.focus, *id);
        assert!(!tree.nodes.iter().any(|(id, _)| *id == geometry_id));
        assert!(rt.state.settings_search_focused());
        assert_eq!(rt.state.settings, before);
    }
}
