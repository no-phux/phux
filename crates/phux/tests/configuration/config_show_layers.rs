//! End-to-end coverage for `phux config show --layers` (phux-r82.4):
//! the provenance view over a real on-disk `extends` stack, in both
//! human and `--json` form.

#![allow(clippy::expect_used, reason = "tests")]

use std::process::Command;

use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
/// The stderr build banner, now a plain `phux <version>` line, matched in
/// full so the absence assertions below stay meaningful.
const BANNER_FRAGMENT: &str = concat!("phux ", env!("CARGO_PKG_VERSION"));

fn run_with_xdg(args: &[&str], xdg_config_home: &std::path::Path) -> (i32, String, String) {
    let out = Command::new(PHUX)
        .env("XDG_CONFIG_HOME", xdg_config_home)
        .args(args)
        .output()
        .expect("run phux binary");
    (
        out.status.code().expect("phux exited via code, not signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Write a two-file stack under `$XDG_CONFIG_HOME/phux`: a distro
/// layer that overrides one leaf and appends a status widget, and a
/// user config that extends it and overrides another leaf.
fn write_stack(tmp: &TempDir) -> std::path::PathBuf {
    let config_dir = tmp.path().join("xdg").join("phux");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    std::fs::write(
        config_dir.join("distro.toml"),
        r#"
[defaults]
history-limit = 2222

[status]
right-append = [{ kind = "time", format = "%H:%M" }]
"#,
    )
    .expect("write distro layer");
    std::fs::write(
        config_dir.join("config.toml"),
        r#"
extends = ["distro.toml"]

[keybindings]
prefix = "C-b"
"#,
    )
    .expect("write config");
    tmp.path().join("xdg")
}

#[test]
fn config_show_layers_attributes_keys_to_layers() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = write_stack(&tmp);

    let (code, stdout, stderr) = run_with_xdg(&["config", "show", "--layers"], &xdg);

    assert_eq!(
        code, 0,
        "`config show --layers` should exit 0; stderr={stderr}"
    );
    assert!(
        !stdout.contains(BANNER_FRAGMENT) && !stderr.contains(BANNER_FRAGMENT),
        "`config show --layers` must not print the banner"
    );
    // Layer stack, in merge order.
    assert!(stdout.contains("[1] defaults (embedded)"), "{stdout}");
    assert!(stdout.contains("distro.toml"), "{stdout}");
    assert!(stdout.contains("(user)"), "{stdout}");
    // Attribution rows: the user's override, the distro's override, an
    // untouched shipped default, and the appended array element.
    let row = |needle: &str| {
        stdout
            .lines()
            .find(|line| line.trim_start().starts_with(needle))
            .unwrap_or_else(|| panic!("row for `{needle}` in:\n{stdout}"))
    };
    assert!(
        row("keybindings.prefix ").contains("<- [3] user"),
        "{stdout}"
    );
    assert!(
        row("defaults.history-limit").contains("<- [2] distro.toml"),
        "{stdout}"
    );
    assert!(row("defaults.term").contains("<- [1] defaults"), "{stdout}");
    // status.right: three shipped elements plus the distro's clock.
    assert!(
        row("status.right[0]").contains("<- [1] defaults"),
        "{stdout}"
    );
    assert!(
        row("status.right[3]").contains("<- [2] distro.toml"),
        "{stdout}"
    );
}

#[test]
fn config_show_layers_json_is_a_stable_document() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = write_stack(&tmp);

    let (code, stdout, stderr) = run_with_xdg(&["config", "show", "--layers", "--json"], &xdg);

    assert_eq!(code, 0, "stderr={stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(value["schema_version"], 1);
    assert!(
        value["config_path"]
            .as_str()
            .expect("config_path is a string")
            .ends_with("phux/config.toml")
    );

    let layers = value["layers"].as_array().expect("layers array");
    assert_eq!(layers.len(), 3);
    assert_eq!(layers[0]["index"], 1);
    assert_eq!(layers[0]["kind"], "defaults");
    assert_eq!(layers[0]["path"], serde_json::Value::Null);
    assert_eq!(layers[1]["kind"], "extended");
    assert!(
        layers[1]["path"]
            .as_str()
            .expect("layer path")
            .ends_with("distro.toml")
    );
    assert_eq!(layers[2]["kind"], "user");

    let keys = value["keys"].as_array().expect("keys array");
    let entry = |key: &str| {
        keys.iter()
            .find(|entry| entry["key"] == key)
            .unwrap_or_else(|| panic!("entry for `{key}`"))
    };
    assert_eq!(entry("keybindings.prefix")["layer"], 3);
    assert_eq!(
        entry("keybindings.prefix")["element_layers"],
        serde_json::Value::Null,
        "scalars carry no element attribution"
    );
    assert_eq!(entry("defaults.history-limit")["layer"], 2);
    assert_eq!(entry("defaults.term")["layer"], 1);
    assert_eq!(
        entry("status.right")["element_layers"],
        serde_json::json!([1, 1, 1, 2])
    );
}

#[test]
fn config_show_layers_without_a_config_file_is_all_defaults() {
    let tmp = TempDir::new().expect("tempdir");
    // No config file at all: every key attributes to the defaults
    // layer and the stack still lists the (absent) user file last.
    let (code, stdout, stderr) = run_with_xdg(
        &["config", "show", "--layers", "--json"],
        &tmp.path().join("xdg"),
    );

    assert_eq!(code, 0, "stderr={stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    let layers = value["layers"].as_array().expect("layers array");
    assert_eq!(layers.len(), 2, "defaults + (empty) user file");
    let keys = value["keys"].as_array().expect("keys array");
    assert!(!keys.is_empty());
    assert!(
        keys.iter().all(|entry| entry["layer"] == 1),
        "every key comes from the defaults layer"
    );
}

#[test]
fn config_show_default_conflicts_with_layers() {
    let tmp = TempDir::new().expect("tempdir");
    let (code, _, stderr) = run_with_xdg(
        &["config", "show", "--default", "--layers"],
        &tmp.path().join("xdg"),
    );
    assert_ne!(code, 0, "--default and --layers must conflict");
    assert!(stderr.contains("--layers"), "{stderr}");
}
