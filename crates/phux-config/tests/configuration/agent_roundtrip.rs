//! Agents can write config: the schema is reachable from JSON, and the TOML
//! round trip of a JSON-expressed config is lossless.

use std::path::Path;

use phux_config::{Config, parse_str};
use serde_json::json;

#[test]
fn agent_can_generate_valid_config() {
    let spec = json!({
        "defaults": {
            "shell": "/bin/zsh",
            "history-limit": 5000
        },
        "keybindings": {
            "prefix": "C-b",
            "prefix-table": {
                "c": "new-window",
                "d": "detach"
            },
            "global": {}
        },
        "status": {
            "left": [],
            "center": [],
            "right": [{ "kind": "time", "format": "%H:%M" }]
        },
        "hooks": {},
        "plugins": [{
            "manifest": "/tmp/phux-plugin.toml",
            "enabled": false
        }],
        "theme": { "fg": "#ddd", "bg": "#111" }
    });

    let cfg: Config = serde_json::from_value(spec).expect("JSON → Config");

    let toml_string = toml::to_string_pretty(&cfg).expect("Config → TOML");

    let reparsed = parse_str(&toml_string, Path::new("roundtrip.toml")).expect("TOML → Config");

    let cfg_json = serde_json::to_value(&cfg).expect("Config → JSON (lhs)");
    let reparsed_json = serde_json::to_value(&reparsed).expect("Config → JSON (rhs)");
    assert_eq!(cfg_json, reparsed_json, "Config round-trip diverged");

    assert_eq!(reparsed.keybindings.prefix, "C-b");
    assert_eq!(reparsed.defaults.history_limit, 5000);
    assert_eq!(reparsed.status.right.len(), 1);
    assert_eq!(reparsed.plugins.len(), 1);
    assert!(!reparsed.plugins[0].enabled);
}
