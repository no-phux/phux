//! Real packages and teaching examples, separate from schema validation fixtures.

use phux_config::plugin;

#[test]
fn theme_example_loads() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("the test runner sets CARGO_MANIFEST_DIR"),
    )
    .join("../../examples/plugins/theme-pack/phux-plugin.toml");
    let loaded = plugin::load_plugin_manifest(&manifest)?;
    assert_eq!(loaded.themes[0].name, "phux-dark");
    assert!(loaded.themes[0].path.join("colors.toml").is_file());
    Ok(())
}

#[test]
fn continuum_plugin_loads() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("the test runner sets CARGO_MANIFEST_DIR"),
    )
    .join("../../plugins/continuum/phux-plugin.toml");
    let loaded = plugin::load_plugin_manifest(&manifest)?;
    assert_eq!(loaded.id, "com.phux.demo.continuum");
    assert_eq!(loaded.actions[0].id, "autosave");
    assert_eq!(loaded.actions[1].id, "restore-latest");
    assert_eq!(loaded.events[0].id, "idle-autosave");
    assert_eq!(loaded.events[1].on, "pane-exit");
    for event in &loaded.events {
        assert!(
            phux_config::vocab::HOOK_EVENTS.contains(&event.on.as_str()),
            "event `{}` listens for unknown `{}`",
            event.id,
            event.on
        );
    }
    assert_eq!(loaded.profiles[0].id, "continuum");
    assert_eq!(loaded.profiles[0].actions, ["autosave", "restore-latest"]);
    assert_eq!(
        loaded.profiles[0].events,
        ["idle-autosave", "session-autosave"]
    );
    Ok(())
}
