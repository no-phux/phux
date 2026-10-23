//! The first-party agent distro is a real, composable config/plugin bundle.
#![allow(clippy::expect_used, reason = "tests")]

use std::path::{Path, PathBuf};

fn layer() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .join("../../distros/crew/crew.toml")
        .canonicalize()
        .expect("Crew layer")
}

fn crew(overrides: &str) -> phux_config::Config {
    let path = layer();
    let encoded = toml::Value::String(path.display().to_string());
    phux_config::parse_with_defaults(
        &format!("extends = [{encoded}]\n{overrides}"),
        Path::new("/not-the-checkout/config.toml"),
    )
    .expect("Crew config loads")
}

#[test]
fn crew_composes_starter_and_loadable_palette_entries() {
    let config = crew("");
    assert_eq!(config.plugins.len(), 4);
    assert_eq!(
        config.theme.slots.get("accent").map(String::as_str),
        Some("#a6f750")
    );
    let manifests: Vec<_> = config
        .plugins
        .iter()
        .map(|entry| {
            assert!(entry.manifest.is_absolute());
            phux_config::plugin::load_plugin_manifest(&entry.manifest).expect("manifest loads")
        })
        .collect();
    let crew = manifests
        .iter()
        .find(|m| m.id == "com.phux.crew")
        .expect("Crew plugin");
    assert_eq!(crew.panes.len(), 2);
    assert!(crew.panes.iter().any(|p| p.id == "lead"));
    assert!(crew.panes.iter().any(|p| p.id == "board"));
    assert!(crew.actions.iter().any(|a| a.id == "doctor"));
    assert!(crew.actions.iter().any(|a| a.id == "pulse"));
    assert!(crew.events.is_empty(), "Crew adds no implicit server hooks");
    let integration = phux_config::integration::load_integration_template(
        &crew.plugin_root.join("integrations/crew.toml"),
    )
    .expect("native Crew launch template");
    assert_eq!(integration.id, "crew");
    assert!(
        integration.agent_identity.is_none(),
        "harness is chosen at launch"
    );
    assert!(
        integration.session_identity.is_none(),
        "no false provider resume claim"
    );
    let launch = integration.launch.expect("launchable");
    assert_eq!(
        launch.command,
        ["python3", "${PHUX_PLUGIN_ROOT}/crew.py", "start"]
    );
}

#[test]
fn personal_overrides_and_explicit_plugin_opt_out_still_win() {
    let config = crew("plugins = []\n[theme]\naccent = 'magenta'\n");
    assert!(config.plugins.is_empty());
    assert_eq!(
        config.theme.slots.get("accent").map(String::as_str),
        Some("magenta")
    );
}
