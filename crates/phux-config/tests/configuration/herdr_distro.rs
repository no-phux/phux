//! The checked-in starter layer (`distros/starter/starter.toml`, formerly
//! herdr) end to end: it is plugin wiring only, its manifests absolutize and
//! load, and a user config overrides it per key.

#![allow(clippy::expect_used, reason = "tests")]

use std::path::{Path, PathBuf};

use phux_config::{Action, Config, parse_with_defaults};

const USER: &str = "/nonexistent-config-dir/config.toml";

fn repo_layer(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
        .canonicalize()
        .expect("layer exists in the repo")
}

/// Parse `user_body` extending `layer`, from a config directory far from it.
fn parse_extending(layer: &Path, user_body: &str) -> Config {
    let user = format!("extends = [\"{}\"]\n{user_body}", layer.display());
    parse_with_defaults(&user, Path::new(USER)).expect("stack parses")
}

fn starter(user_body: &str) -> Config {
    parse_extending(&repo_layer("distros/starter/starter.toml"), user_body)
}

/// The opinions the layer used to carry are shipped defaults now, and
/// extending it changes nothing but the plugin set.
#[test]
fn the_curated_opinions_are_shipped_defaults() {
    let bare = parse_with_defaults("", Path::new(USER)).expect("defaults parse");
    assert!(bare.keybindings.which_key);
    assert_eq!(bare.keybindings.which_key_delay_ms, 400);
    let table = &bare.keybindings.prefix_table;
    assert_eq!(
        table.get("Space"),
        Some(&Action::Bare("command-palette".to_owned()))
    );
    assert_eq!(
        table.get("Tab"),
        Some(&Action::Bare("next-window".to_owned()))
    );
    for chord in ["|", "-", "c", "%", ":"] {
        assert!(table.contains_key(chord), "{chord}");
    }
    assert_eq!(bare.defaults.session_name_template, "${cwd-basename}");
    assert_eq!(
        (
            bare.status.left.len(),
            bare.status.center.len(),
            bare.status.right.len()
        ),
        (1, 0, 3)
    );

    let cfg = starter("");
    assert_eq!(cfg.keybindings, bare.keybindings);
    assert_eq!(cfg.defaults, bare.defaults);
    assert_eq!(cfg.status, bare.status);
    assert_eq!(cfg.theme, bare.theme);
    assert_eq!(cfg.plugins.len(), 2, "{:?}", cfg.plugins);
}

/// Layer-relative manifests become absolute, normalized, existing paths
/// that load; the pre-rename herdr stub wires the same set.
#[test]
fn plugin_manifests_absolutize_and_load() {
    let manifests: Vec<_> = starter("")
        .plugins
        .into_iter()
        .map(|p| p.manifest)
        .collect();
    assert!(manifests[0].ends_with("examples/plugins/continuum/phux-plugin.toml"));
    assert!(manifests[1].ends_with("examples/plugins/agent-tools/phux-plugin.toml"));
    for manifest in &manifests {
        assert!(manifest.is_absolute(), "{manifest:?}");
        assert!(
            !manifest
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
            "{manifest:?}"
        );
        phux_config::plugin::load_plugin_manifest(manifest).expect("wired manifest loads");
    }

    let via_stub = parse_extending(&repo_layer("distros/herdr/herdr.toml"), "");
    let stub_manifests: Vec<_> = via_stub.plugins.into_iter().map(|p| p.manifest).collect();
    assert_eq!(stub_manifests, manifests);
}

/// User leaves beat the layer, `-append` composes with its appends, and a
/// plain assignment opts out of the distro set entirely.
#[test]
fn user_overrides_win_and_appends_compose() {
    let cfg = starter(
        r#"
[keybindings]
which-key-delay-ms = 800

[keybindings.prefix-table]
"Space" = "show-help"

[theme]
accent = "magenta"

[[plugins-append]]
manifest = "/home/me/extra/phux-plugin.toml"
"#,
    );
    assert_eq!(cfg.keybindings.which_key_delay_ms, 800);
    assert_eq!(
        cfg.keybindings.prefix_table.get("Space"),
        Some(&Action::Bare("show-help".to_owned()))
    );
    assert_eq!(
        cfg.theme.slots.get("accent").map(String::as_str),
        Some("magenta")
    );
    assert_eq!(cfg.plugins.len(), 3);
    assert_eq!(
        cfg.plugins[2].manifest,
        Path::new("/home/me/extra/phux-plugin.toml")
    );

    assert!(starter("plugins = []\n").plugins.is_empty());
}
