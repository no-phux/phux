#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

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

fn write_manifest(plugin_dir: &std::path::Path, id: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(plugin_dir).expect("create plugin dir");
    let manifest = plugin_dir.join("phux-plugin.toml");
    std::fs::write(
        &manifest,
        format!(
            r#"
id = "{id}"
name = "Lifecycle"
version = "0.1.0"
min_phux_version = "0.0.2"

[[actions]]
id = "open"
title = "Open"
command = ["true"]

[[events]]
id = "idle"
title = "Pane idle"
on = "pane.idle"
command = ["true"]

[[panes]]
id = "board"
title = "Agent Board"
placement = "split"
command = ["true"]

[[links]]
id = "ticket"
title = "Open ticket"
contexts = ["pane"]
patterns = ["https://linear.app/*"]
command = ["true"]
"#
        ),
    )
    .expect("write manifest");
    manifest
}

#[test]
fn link_list_disable_enable_unlink_json_is_machine_readable() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let manifest = write_manifest(&tmp.path().join("plugin"), "example.lifecycle");
    let manifest_arg = manifest.to_string_lossy();

    let (code, stdout, stderr) = run_with_xdg(&["plugin", "link", &manifest_arg, "--json"], &xdg);
    assert_eq!(
        code, 0,
        "`plugin link --json` should exit 0; stderr={stderr}"
    );
    assert!(
        !stdout.contains(BANNER_FRAGMENT) && !stderr.contains(BANNER_FRAGMENT),
        "`plugin link --json` must be banner-free; stdout={stdout:?} stderr={stderr:?}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("link stdout is JSON");
    assert_eq!(value["plugin"]["id"], "example.lifecycle");
    assert_eq!(value["plugin"]["enabled"], true);
    assert!(xdg.join("phux").join("config.toml").exists());

    let (code, stdout, stderr) = run_with_xdg(&["plugin", "list", "--json"], &xdg);
    assert_eq!(
        code, 0,
        "`plugin list --json` should exit 0; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("list stdout is JSON");
    assert_eq!(value["plugins"][0]["id"], "example.lifecycle");
    assert_eq!(value["plugins"][0]["events"][0]["id"], "idle");
    assert_eq!(value["plugins"][0]["panes"][0]["id"], "board");
    assert_eq!(value["plugins"][0]["links"][0]["id"], "ticket");

    let (code, stdout, stderr) =
        run_with_xdg(&["plugin", "disable", "example.lifecycle", "--json"], &xdg);
    assert_eq!(
        code, 0,
        "`plugin disable --json` should exit 0; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("disable stdout is JSON");
    assert_eq!(value["plugin"]["enabled"], false);

    let (code, stdout, stderr) =
        run_with_xdg(&["plugin", "enable", "example.lifecycle", "--json"], &xdg);
    assert_eq!(
        code, 0,
        "`plugin enable --json` should exit 0; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("enable stdout is JSON");
    assert_eq!(value["plugin"]["enabled"], true);

    let (code, stdout, stderr) =
        run_with_xdg(&["plugin", "unlink", "example.lifecycle", "--json"], &xdg);
    assert_eq!(
        code, 0,
        "`plugin unlink --json` should exit 0; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("unlink stdout is JSON");
    assert_eq!(value["removed"]["id"], "example.lifecycle");

    let (code, stdout, stderr) = run_with_xdg(&["plugin", "list", "--json"], &xdg);
    assert_eq!(
        code, 0,
        "`plugin list --json` should exit 0; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("list stdout is JSON");
    assert_eq!(value["plugins"].as_array().expect("plugins array").len(), 0);
}

#[test]
fn relative_manifest_entries_survive_toggle() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let config_dir = xdg.join("phux");
    let plugin_dir = config_dir.join("plugins").join("relative");
    write_manifest(&plugin_dir, "example.relative-lifecycle");
    std::fs::write(
        config_dir.join("config.toml"),
        r#"
[[plugins]]
manifest = "./plugins/relative/phux-plugin.toml"
enabled = true
"#,
    )
    .expect("write config");

    let (code, stdout, stderr) = run_with_xdg(
        &["plugin", "disable", "example.relative-lifecycle", "--json"],
        &xdg,
    );
    assert_eq!(
        code, 0,
        "`plugin disable --json` should exit 0 for relative manifests; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("disable stdout is JSON");
    assert_eq!(
        value["plugin"]["manifest"],
        "./plugins/relative/phux-plugin.toml"
    );
    assert_eq!(value["plugin"]["enabled"], false);

    let config = std::fs::read_to_string(config_dir.join("config.toml")).expect("read config");
    assert!(config.contains(r#"manifest = "./plugins/relative/phux-plugin.toml""#));
    assert!(config.contains("enabled = false"));
}

#[test]
fn duplicate_registry_plugin_ids_are_rejected() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let config_dir = xdg.join("phux");
    let first = write_manifest(
        &config_dir.join("plugins").join("first"),
        "example.duplicate",
    );
    let second = write_manifest(
        &config_dir.join("plugins").join("second"),
        "example.duplicate",
    );
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            r#"
[[plugins]]
manifest = "{}"
enabled = true

[[plugins]]
manifest = "{}"
enabled = true
"#,
            first.display(),
            second.display()
        ),
    )
    .expect("write config");

    let (code, stdout, stderr) = run_with_xdg(&["plugin", "disable", "example.duplicate"], &xdg);

    assert_ne!(code, 0, "duplicate plugin ids should be refused");
    assert!(stdout.is_empty());
    assert!(stderr.contains(r#"duplicate plugin id "example.duplicate""#));
}

#[test]
fn validate_json_reports_missing_manifest_without_stdout() {
    let tmp = TempDir::new().expect("tempdir");
    let missing = tmp.path().join("missing").join("phux-plugin.toml");
    let missing_arg = missing.to_string_lossy();

    let (code, stdout, stderr) =
        run_with_xdg(&["plugin", "validate", &missing_arg, "--json"], tmp.path());

    assert_ne!(
        code, 0,
        "`plugin validate --json` should fail for missing manifest"
    );
    assert!(stdout.is_empty());
    assert!(!stderr.contains(BANNER_FRAGMENT));
    assert!(stderr.contains("could not load") && stderr.contains("missing"));
}

#[test]
#[cfg(unix)]
fn lifecycle_refuses_to_overwrite_symlinked_config() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let config_dir = xdg.join("phux");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    let victim = tmp.path().join("victim.toml");
    std::fs::write(&victim, "do-not-touch").expect("write victim");
    std::os::unix::fs::symlink(&victim, config_dir.join("config.toml")).expect("symlink config");
    let manifest = write_manifest(&tmp.path().join("plugin"), "example.symlink");
    let manifest_arg = manifest.to_string_lossy();

    let (code, stdout, stderr) = run_with_xdg(&["plugin", "link", &manifest_arg, "--json"], &xdg);

    assert_ne!(code, 0, "symlinked config should be refused");
    assert!(stdout.is_empty());
    assert!(stderr.contains("must not be a symlink"));
    assert_eq!(
        std::fs::read_to_string(victim).expect("read victim"),
        "do-not-touch"
    );
}
