//! Real packages and teaching examples, separate from schema validation fixtures.

use phux_config::plugin;

fn repo_manifest(relative: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

#[test]
fn theme_example_loads() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = repo_manifest("examples/plugins/theme-pack/phux-plugin.toml");
    let loaded = plugin::load_plugin_manifest(&manifest)?;
    assert_eq!(loaded.themes[0].name, "phux-dark");
    assert!(loaded.themes[0].path.join("colors.toml").is_file());
    Ok(())
}

/// Every first-party plugin's hooks listen for events the server fires.
#[test]
fn first_party_plugins_hook_real_server_events() -> Result<(), Box<dyn std::error::Error>> {
    for name in ["continuum", "agent-stats", "usage"] {
        let manifest = repo_manifest(&format!("plugins/{name}/phux-plugin.toml"));
        let loaded = plugin::load_plugin_manifest(&manifest)?;
        assert!(!loaded.events.is_empty(), "{name}");
        let dead = plugin::dead_events(&loaded);
        assert!(dead.is_empty(), "{name}: {dead:?}");
    }
    Ok(())
}
