//! Exercise switching through the real CLI with isolated config and no server.

#![allow(clippy::expect_used, reason = "tests")]

use std::path::Path;

fn run(root: &Path, args: &[&str]) -> std::process::Output {
    crate::common::phux_cmd(crate::runner::phux_bin())
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("PHUX_DISTROS_DIR", root.join("distros"))
        .args(args)
        .output()
        .expect("run CLI")
}

fn document(root: &Path, args: &[&str]) -> serde_json::Value {
    let output = run(root, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON")
}

#[test]
fn distro_cli_switch_current_off_and_failures_preserve_personal_config() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let layer = root.join("distros/local/local.toml");
    std::fs::create_dir_all(layer.parent().expect("parent")).expect("mkdir");
    std::fs::write(&layer, "[defaults]\nhistory-limit = 123\n").expect("distro");
    let config = root.join("config/phux/config.toml");
    std::fs::create_dir_all(config.parent().expect("parent")).expect("mkdir");
    let personal = "# mine\n[defaults]\nhistory-limit = 789 # keep\n";
    std::fs::write(&config, personal).expect("config");
    let list = document(root, &["config", "distro", "list", "--json"]);
    assert!(
        list["distros"]
            .as_array()
            .expect("array")
            .iter()
            .any(|d| d["name"] == "local")
    );
    assert!(document(root, &["config", "distro", "current", "--json"])["distro"].is_null());
    let selected = document(root, &["config", "distro", "use", "local", "--json"]);
    assert_eq!(
        selected["distro"],
        layer
            .canonicalize()
            .expect("canonical")
            .to_str()
            .expect("utf8")
    );
    assert_eq!(selected["reload_required"], true);
    assert!(
        selected["message"]
            .as_str()
            .expect("message")
            .contains("config reload")
    );
    let current = document(root, &["config", "distro", "current", "--json"]);
    assert_eq!(current["distro"], selected["distro"]);
    let selected_text = std::fs::read_to_string(&config).expect("read");
    for name in ["no-such-distro", "..", "bad\\name"] {
        let failure = run(root, &["config", "distro", "use", name, "--json"]);
        assert!(!failure.status.success());
        assert!(failure.stdout.is_empty());
        let failure: serde_json::Value =
            serde_json::from_slice(&failure.stderr).expect("JSON error");
        assert_eq!(failure["error"]["code"], "invalid_config");
        assert_eq!(
            std::fs::read_to_string(&config).expect("read"),
            selected_text
        );
    }
    std::fs::write(&layer, "[sidebar]\nwidht = 3\n").expect("break distro");
    let failed = run(root, &["config", "distro", "use", "local"]);
    assert!(!failed.status.success());
    assert_eq!(
        std::fs::read_to_string(&config).expect("read"),
        selected_text
    );
    let off = document(root, &["config", "distro", "off", "--json"]);
    assert!(off["distro"].is_null());
    assert!(
        std::fs::read_to_string(&config)
            .expect("read")
            .contains(personal)
    );
    let output = run(
        root,
        &["config", "distro", "use", layer.to_str().expect("path")],
    );
    assert!(!output.status.success());
}

#[test]
fn distro_switch_accepts_custom_paths_and_teaches_explicit_reload() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let layer = tmp.path().join("custom.toml");
    std::fs::write(&layer, "[sidebar]\nwidth = 30\n").expect("custom distro");
    let output = run(
        tmp.path(),
        &["config", "distro", "use", layer.to_str().expect("path")],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("config reload"), "{stdout}");
    assert!(stdout.contains("next server start"), "{stdout}");
    let off = run(tmp.path(), &["config", "distro", "off"]);
    assert!(off.status.success());
    assert!(String::from_utf8_lossy(&off.stdout).starts_with("off\n"));
}
