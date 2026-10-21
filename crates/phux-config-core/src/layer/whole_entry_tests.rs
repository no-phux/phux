//! Binding tables replace whole entries; other tables merge per key.

use super::merged_with_budget;
use std::path::Path;

#[test]
fn a_rebound_key_drops_the_replaced_bindings_arguments() {
    let defaults =
        "[keybindings.prefix-table]\n\"0\" = { action = \"select-window\", index = 0 }\n";
    let user = "[keybindings.prefix-table]\n\"0\" = { action = \"toggle-zoom\" }\n";
    let (merged, provenance) =
        merged_with_budget(defaults, user, Path::new("config.toml"), None).expect("merges");
    let entry = merged["keybindings"]["prefix-table"]["0"]
        .as_table()
        .expect("table binding");
    assert_eq!(
        entry.get("action").and_then(toml::Value::as_str),
        Some("toggle-zoom")
    );
    assert!(
        !entry.contains_key("index"),
        "stale argument leaked: {entry:?}"
    );
    assert!(
        !provenance
            .keys
            .contains_key("keybindings.prefix-table.\"0\".index")
    );
}

#[test]
fn other_tables_still_merge_per_key() {
    let defaults = "[defaults]\nmouse = true\nterm = \"x\"\n";
    let user = "[defaults]\nmouse = false\n";
    let (merged, _) =
        merged_with_budget(defaults, user, Path::new("config.toml"), None).expect("merges");
    assert_eq!(merged["defaults"]["term"].as_str(), Some("x"));
    assert_eq!(merged["defaults"]["mouse"].as_bool(), Some(false));
}
