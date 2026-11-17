//! Actual ACP choices rely on upstream's presentational repeated captions.
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
fn acp_choices_expose_one_name_and_keep_checkbox_radio_activation() {
    let (client, mut rt) = fixture((320.0, 568.0), "ask");
    for (item, role) in [("false", "radio-button"), ("true", "check-box")] {
        let key = format!("acp-{ID}-{item}-one");
        reveal(&mut rt, &key);
        let tree = rt.accessibility_json();
        let node = keyed(&tree, &key).unwrap();
        assert_eq!(node["role"], role);
        assert_eq!(node["name"], labelled(&option("one")));
        assert!(
            node.get("children").is_none(),
            "the repeated caption is presentational, not spoken twice"
        );
        assert!(
            node["states"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "unchecked")
        );
        assert!(
            node["actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "click")
        );
        assert!(rt.accessibility_action(&key, rune::accesskit::Action::Click, None));
        settle(&mut rt);
        assert_eq!(rt.state.inbox.questions.drafts[ID].choices[item], ["one"]);
        let tree = rt.accessibility_json();
        let node = keyed(&tree, &key).unwrap();
        assert!(
            node["states"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "checked")
        );
        no_input(&client);
    }
}
