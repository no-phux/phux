//! Integration tests for layered config resolution (ADR-0039).
//!
//! Covers:
//! 1. A three-layer stack (defaults <- base layer <- distro layer <-
//!    user) merges leaf-by-leaf with later layers winning.
//! 2. Array semantics: plain keys replace wholesale; `-append` keys
//!    append — for `[[plugins]]`, status widget slots, and
//!    `[[hooks.<name>]]`.
//! 3. A missing layer file is an error naming the layer AND the file
//!    that referenced it.
//! 4. An `extends` cycle is an error naming the offending edge.
//! 5. Nesting past `MAX_EXTENDS_DEPTH` is an error.
//! 6. Guard-rail errors: non-array `extends`, `x` + `x-append` in one
//!    layer, `-append` with a non-array value.
//! 7. Bare-name entries resolve to `layers/<name>.toml`; diamonds
//!    merge once.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::fs;
use std::path::{Path, PathBuf};

use phux_config::{ConfigError, MAX_EXTENDS_DEPTH, loader, parse_with_defaults};
use tempfile::TempDir;

/// Write `contents` to `dir/name`, creating parent directories.
fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(&path, contents).expect("write layer");
    path
}

#[test]
fn three_layer_merge_later_layers_win_per_leaf() {
    let tmp = TempDir::new().expect("tempdir");
    write(
        tmp.path(),
        "base.toml",
        r#"
[defaults]
history-limit = 1111
mouse         = false

[keybindings.prefix-table]
"b" = "new-window"
"#,
    );
    write(
        tmp.path(),
        "distro.toml",
        r#"
extends = ["base.toml"]

[defaults]
history-limit = 2222

[keybindings.prefix-table]
"g" = "detach"
"#,
    );
    let user = r#"
extends = ["distro.toml"]

[keybindings]
prefix = "C-b"
"#;

    let cfg = parse_with_defaults(user, &tmp.path().join("config.toml")).expect("layered parse");

    // User leaf wins.
    assert_eq!(cfg.keybindings.prefix, "C-b");
    // Distro overrides base.
    assert_eq!(cfg.defaults.history_limit, 2222);
    // Base leaf survives where nothing above touches it.
    assert!(!cfg.defaults.mouse);
    // Prefix-table entries from both layers coexist (tables merge per
    // key), alongside the shipped defaults.
    assert!(cfg.keybindings.prefix_table.contains_key("b"));
    assert!(cfg.keybindings.prefix_table.contains_key("g"));
    assert!(
        cfg.keybindings.prefix_table.contains_key("c"),
        "shipped default binding must survive the stack"
    );
}

#[test]
fn plain_array_replaces_wholesale() {
    let tmp = TempDir::new().expect("tempdir");
    write(
        tmp.path(),
        "distro.toml",
        r#"
[status]
right = ["session-name", { kind = "time", format = "%H:%M" }]
"#,
    );
    let user = r#"
extends = ["distro.toml"]

[status]
right = ["session-name"]
"#;

    let cfg = parse_with_defaults(user, &tmp.path().join("config.toml")).expect("layered parse");
    // The user's plain assignment replaces the distro's two-widget list.
    assert_eq!(cfg.status.right.len(), 1);
}

#[test]
fn append_composes_plugins_widgets_and_hooks_across_layers() {
    let tmp = TempDir::new().expect("tempdir");
    write(
        tmp.path(),
        "distro.toml",
        r#"
[[plugins-append]]
manifest = "/opt/distro/phux-plugin.toml"

[status]
right-append = [{ kind = "time", format = "%H:%M" }]

[[hooks.pane-exit-append]]
when   = { exit-code = 0 }
action = "noop"
"#,
    );
    let user = r#"
extends = ["distro.toml"]

[[plugins-append]]
manifest = "/home/me/phux-plugin.toml"

[[hooks.pane-exit-append]]
when   = { exit-code = "*" }
action = "noop"
"#;

    let cfg = parse_with_defaults(user, &tmp.path().join("config.toml")).expect("layered parse");

    // Both layers' plugin entries survive, in stack order.
    let manifests: Vec<_> = cfg
        .plugins
        .iter()
        .map(|p| p.manifest.display().to_string())
        .collect();
    assert_eq!(
        manifests,
        vec!["/opt/distro/phux-plugin.toml", "/home/me/phux-plugin.toml"]
    );

    // The distro's clock is appended after the shipped default right
    // slot rather than replacing it.
    let shipped_right = phux_config::parse_with_defaults("", Path::new("empty.toml"))
        .expect("defaults")
        .status
        .right;
    assert_eq!(cfg.status.right.len(), shipped_right.len() + 1);

    // Hooks: shipped defaults declare none; distro + user contribute
    // one `pane-exit` entry each.
    assert_eq!(cfg.hooks.get("pane-exit").map(Vec::len), Some(2));
}

#[test]
fn missing_layer_file_names_layer_and_referencing_file() {
    let tmp = TempDir::new().expect("tempdir");
    let user = r#"extends = ["nope.toml"]"#;
    let user_path = tmp.path().join("config.toml");

    let err = parse_with_defaults(user, &user_path).expect_err("missing layer must fail");
    match &err {
        ConfigError::LayerRead {
            layer,
            referenced_from,
            ..
        } => {
            assert_eq!(layer, &tmp.path().join("nope.toml"));
            assert_eq!(referenced_from, &user_path);
        }
        other => panic!("expected LayerRead, got: {other:?}"),
    }
    let msg = err.to_string();
    assert!(msg.contains("nope.toml"), "error names the layer: {msg}");
    assert!(
        msg.contains("config.toml"),
        "error names the referencing file: {msg}"
    );
}

#[test]
fn extends_cycle_is_an_error() {
    let tmp = TempDir::new().expect("tempdir");
    write(tmp.path(), "a.toml", r#"extends = ["b.toml"]"#);
    write(tmp.path(), "b.toml", r#"extends = ["a.toml"]"#);
    let user = r#"extends = ["a.toml"]"#;

    let err =
        parse_with_defaults(user, &tmp.path().join("config.toml")).expect_err("cycle must fail");
    match &err {
        ConfigError::LayerCycle {
            layer,
            referenced_from,
        } => {
            assert!(layer.ends_with("a.toml"), "cycle closes at a.toml: {err}");
            assert!(referenced_from.ends_with("b.toml"));
        }
        other => panic!("expected LayerCycle, got: {other:?}"),
    }
}

#[test]
fn self_extends_is_a_cycle() {
    let tmp = TempDir::new().expect("tempdir");
    write(tmp.path(), "a.toml", r#"extends = ["a.toml"]"#);
    let user = r#"extends = ["a.toml"]"#;

    let err = parse_with_defaults(user, &tmp.path().join("config.toml"))
        .expect_err("self-cycle must fail");
    assert!(matches!(err, ConfigError::LayerCycle { .. }), "{err:?}");
}

#[test]
fn nesting_past_max_depth_is_an_error() {
    let tmp = TempDir::new().expect("tempdir");
    // Chain: config -> d1 -> d2 -> ... -> d(MAX+1). The file at depth
    // MAX declares `extends`, which is one level too deep.
    for i in 1..=MAX_EXTENDS_DEPTH + 1 {
        let body = if i <= MAX_EXTENDS_DEPTH {
            format!("extends = [\"d{}.toml\"]\n", i + 1)
        } else {
            String::new()
        };
        write(tmp.path(), &format!("d{i}.toml"), &body);
    }
    let user = r#"extends = ["d1.toml"]"#;

    let err = parse_with_defaults(user, &tmp.path().join("config.toml"))
        .expect_err("depth overflow must fail");
    match &err {
        ConfigError::Layer { path, message } => {
            assert!(
                path.ends_with(format!("d{MAX_EXTENDS_DEPTH}.toml")),
                "names the file that nests too deep: {err}"
            );
            assert!(message.contains("depth"), "{message}");
        }
        other => panic!("expected Layer, got: {other:?}"),
    }
}

#[test]
fn extends_must_be_an_array_of_strings() {
    let err = parse_with_defaults(r#"extends = "distro.toml""#, Path::new("config.toml"))
        .expect_err("non-array extends must fail");
    assert!(
        matches!(&err, ConfigError::Layer { message, .. } if message.contains("array of strings")),
        "{err:?}"
    );

    let err = parse_with_defaults("extends = [1, 2]", Path::new("config.toml"))
        .expect_err("non-string entries must fail");
    assert!(matches!(err, ConfigError::Layer { .. }), "{err:?}");
}

#[test]
fn replace_and_append_in_the_same_layer_is_an_error() {
    let user = r#"
[status]
right = ["session-name"]
right-append = ["session-name"]
"#;
    let err = parse_with_defaults(user, Path::new("config.toml"))
        .expect_err("conflicting directives must fail");
    match &err {
        ConfigError::Layer { path, message } => {
            assert!(path.ends_with("config.toml"));
            assert!(message.contains("right"), "{message}");
        }
        other => panic!("expected Layer, got: {other:?}"),
    }
}

#[test]
fn append_with_non_array_value_is_an_error() {
    let user = r#"
[status]
right-append = "session-name"
"#;
    let err =
        parse_with_defaults(user, Path::new("config.toml")).expect_err("scalar -append must fail");
    assert!(
        matches!(&err, ConfigError::Layer { message, .. } if message.contains("must be an array")),
        "{err:?}"
    );
}

#[test]
fn append_targeting_a_non_array_is_an_error() {
    // `defaults` is a table in the shipped defaults.
    let user = "defaults-append = []\n";
    let err = parse_with_defaults(user, Path::new("config.toml"))
        .expect_err("appending to a table must fail");
    assert!(
        matches!(&err, ConfigError::Layer { message, .. } if message.contains("not an array")),
        "{err:?}"
    );
}

#[test]
fn bare_name_resolves_to_layers_subdirectory() {
    let tmp = TempDir::new().expect("tempdir");
    write(
        tmp.path(),
        "layers/minimal.toml",
        r"
[defaults]
history-limit = 7777
",
    );
    let user = r#"extends = ["minimal"]"#;

    let cfg = parse_with_defaults(user, &tmp.path().join("config.toml")).expect("bare name");
    assert_eq!(cfg.defaults.history_limit, 7777);
}

#[test]
fn diamond_layer_merges_once() {
    let tmp = TempDir::new().expect("tempdir");
    write(
        tmp.path(),
        "shared.toml",
        r#"
[[plugins-append]]
manifest = "/opt/shared/phux-plugin.toml"
"#,
    );
    write(tmp.path(), "a.toml", r#"extends = ["shared.toml"]"#);
    write(tmp.path(), "b.toml", r#"extends = ["shared.toml"]"#);
    let user = r#"extends = ["a.toml", "b.toml"]"#;

    let cfg = parse_with_defaults(user, &tmp.path().join("config.toml")).expect("diamond");
    // The shared layer's append applies exactly once, even though it
    // is reachable through both branches.
    assert_eq!(cfg.plugins.len(), 1);
}

#[test]
fn extended_layer_manifests_resolve_relative_to_the_layer_file() {
    // A distro layer wires plugins with paths relative to ITSELF; plugin
    // loaders resolve relative manifests against the user config's
    // directory, so layer resolution must absolutize them (phux-r82.9).
    let tmp = TempDir::new().expect("tempdir");
    write(
        tmp.path(),
        "distro/distro.toml",
        r#"
[[plugins-append]]
manifest = "/already/absolute/phux-plugin.toml"

[[plugins-append]]
manifest = "plugins/one/phux-plugin.toml"
"#,
    );
    let user = r#"extends = ["distro/distro.toml"]"#;
    // The user config lives at the tempdir root — NOT in distro/ — so a
    // path that leaked through unrewritten would resolve wrongly.
    let cfg = parse_with_defaults(user, &tmp.path().join("config.toml")).expect("layered parse");

    assert_eq!(cfg.plugins.len(), 2);
    assert_eq!(
        cfg.plugins[0].manifest,
        Path::new("/already/absolute/phux-plugin.toml"),
        "absolute manifests pass through untouched"
    );
    assert_eq!(
        cfg.plugins[1].manifest,
        tmp.path()
            .join("distro")
            .join("plugins/one/phux-plugin.toml"),
        "relative manifests absolutize against the layer's directory"
    );
}

#[test]
fn root_config_manifests_are_left_verbatim() {
    // Only *extended* layers are rewritten: the root file's relative
    // manifests keep the documented resolve-against-config-dir behavior
    // (and `phux config show` keeps echoing what the user wrote).
    let user = r#"
[[plugins]]
manifest = "plugins/mine/phux-plugin.toml"
"#;
    let cfg = parse_with_defaults(user, Path::new("/tmp/config.toml")).expect("parse");
    assert_eq!(
        cfg.plugins[0].manifest,
        Path::new("plugins/mine/phux-plugin.toml")
    );
}

#[test]
fn loader_resolves_extends_relative_to_the_config_file() {
    let tmp = TempDir::new().expect("tempdir");
    write(
        tmp.path(),
        "distro.toml",
        r#"
[keybindings]
prefix = "C-x"
"#,
    );
    let config_path = write(tmp.path(), "config.toml", r#"extends = ["distro.toml"]"#);

    let cfg = loader::load_from(&config_path).expect("loader resolves layers");
    assert_eq!(cfg.keybindings.prefix, "C-x");
}

#[test]
fn no_extends_means_no_layer_io_and_unchanged_two_layer_behavior() {
    // A path whose parent does not exist: if layer resolution did any
    // I/O for a plain config, this would fail.
    let cfg = parse_with_defaults(
        "[defaults]\nhistory-limit = 42\n",
        Path::new("/nonexistent-dir/config.toml"),
    )
    .expect("plain config needs no filesystem");
    assert_eq!(cfg.defaults.history_limit, 42);
}
