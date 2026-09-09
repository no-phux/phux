//! Agent round-trip test.
//!
//! Validates the "agents can read/write config" premise of `phux-nz4.2`:
//! an agent produces a config as JSON, serde converts it to the typed
//! [`Config`], we serialize that to TOML, re-parse via the loader's
//! [`parse_str`], and confirm the two `Config`s are equivalent under
//! `serde_json::Value` equality.
//!
//! This pins down two invariants:
//! 1. The schema is reachable from JSON (no TOML-only quirks block agents).
//! 2. TOML round-trip is lossless for any JSON-expressible config.

use std::path::Path;

use phux_config::{Config, parse_str};
use serde_json::json;

#[test]
fn agent_can_generate_valid_config() {
    // 1. Agent produces a config as JSON. Field names match the schema's
    //    canonical (serde-rename) spelling — kebab-case for the keys that
    //    use `rename = "kebab-case"` in `schema.rs`, snake_case elsewhere.
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

    // 2. Deserialize JSON into Config via serde.
    let cfg: Config = serde_json::from_value(spec).expect("JSON → Config");

    // 3. Serialize Config to TOML.
    let toml_string = toml::to_string_pretty(&cfg).expect("Config → TOML");

    // 4. Re-parse TOML into Config via the loader's parse_str.
    let reparsed = parse_str(&toml_string, Path::new("roundtrip.toml")).expect("TOML → Config");

    // 5. JSON-equivalent representations must match — implies the
    //    round-trip is lossless.
    let cfg_json = serde_json::to_value(&cfg).expect("Config → JSON (lhs)");
    let reparsed_json = serde_json::to_value(&reparsed).expect("Config → JSON (rhs)");
    assert_eq!(cfg_json, reparsed_json, "Config round-trip diverged");

    // Spot-check a couple of fields so a regression doesn't pass silently.
    assert_eq!(reparsed.keybindings.prefix, "C-b");
    assert_eq!(reparsed.defaults.history_limit, 5000);
    assert_eq!(reparsed.status.right.len(), 1);
    assert_eq!(reparsed.plugins.len(), 1);
    assert!(!reparsed.plugins[0].enabled);
}
