//! Rune owns repeated switch-label suppression; descriptions stay explicit.
use super::*;

fn keyed<'a>(tree: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    match tree {
        serde_json::Value::Object(fields)
            if fields.get("key").is_some_and(|value| value == key) =>
        {
            Some(tree)
        }
        serde_json::Value::Object(fields) => fields.values().find_map(|value| keyed(value, key)),
        serde_json::Value::Array(values) => values.iter().find_map(|value| keyed(value, key)),
        _ => None,
    }
}

#[test]
fn settings_switches_expose_one_name_and_keep_description_state_and_action() {
    for (phone, size) in [(false, (640.0, 360.0)), (true, (393.0, 852.0))] {
        let mut rt = runtime(size, phone);
        rt.update(|d, tx| {
            d.settings_ui.category = Some(Category::Terminal);
            tx.instant();
            tx.scroll_into_view("set-copy-select");
        });
        rt.frame(1.0);
        rt.frame(1.0);
        let tree = rt.accessibility_json();
        let node = keyed(&tree, "set-copy-select").unwrap();
        assert_eq!(node["role"], "switch");
        assert_eq!(node["name"], "Copy on select");
        assert_eq!(
            tree.to_string()
                .matches("\"name\":\"Copy on select\"")
                .count(),
            1
        );
        let detail = "Finishing a selection puts it on the clipboard.";
        assert_eq!(node["description"], detail);
        assert_eq!(tree.to_string().matches(detail).count(), 1);
        assert!(
            node["actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "click")
        );
        let before = rt.state.settings.copy_on_select;
        assert!(rt.accessibility_action("set-copy-select", Action::Click, None));
        rt.frame(1.0);
        assert_eq!(rt.state.settings.copy_on_select, !before);
        let tree = rt.accessibility_json();
        let node = keyed(&tree, "set-copy-select").unwrap();
        let state = if before { "unchecked" } else { "checked" };
        assert!(
            node["states"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == state)
        );
    }
}

#[test]
fn theme_is_one_named_button_with_its_current_value_and_help_on_both_forms() {
    for phone in [false, true] {
        let mut rt = runtime((640.0, 720.0), phone);
        rt.update(|d, tx| {
            d.settings_category(Category::Appearance);
            d.settings.theme = crate::theme::find("phux-light");
            tx.instant();
        });
        rt.frame(1.0);
        let tree = rt.accessibility_json();
        let button = keyed(&tree, "settings-theme").unwrap();
        assert_eq!(button["role"], "button");
        assert_eq!(button["name"], "Theme");
        assert_eq!(button["value"], "phux Light");
        assert_eq!(
            button["description"],
            "System, built-in and local themes. Preview live, Return to keep, Escape to restore."
        );
        assert_eq!(tree.to_string().matches("\"name\":\"Theme\"").count(), 1);
        assert!(rt.accessibility_action("settings-theme", Action::Click, None));
        rt.frame(1.0);
        assert!(rt.state.palette_open);
    }
}
