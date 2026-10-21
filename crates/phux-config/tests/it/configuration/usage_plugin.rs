//! The first-party usage plugin's manifest: hooks on events the server
//! fires, and the script they run, checked in beside it.

use phux_config::plugin;

#[test]
fn the_usage_plugin_hooks_real_server_events() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("the test runner sets CARGO_MANIFEST_DIR"),
    )
    .join("../../examples/plugins/usage/phux-plugin.toml");
    let loaded = plugin::load_plugin_manifest(&manifest)?;
    assert_eq!(loaded.id, "com.phux.usage");
    let hooks: Vec<(&str, &str)> = loaded
        .events
        .iter()
        .map(|event| (event.id.as_str(), event.on.as_str()))
        .collect();
    assert_eq!(
        hooks,
        [
            ("publish-on-attach", "client-attached"),
            ("publish-on-agent-state", "agent-state-changed"),
        ],
        "the hooks name events the server fires"
    );
    assert!(loaded.plugin_root.join("scripts/publish.sh").is_file());
    let actions: Vec<&str> = loaded.actions.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(actions, ["refresh", "show"]);
    Ok(())
}
