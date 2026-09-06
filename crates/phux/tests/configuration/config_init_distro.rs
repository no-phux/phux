//! `phux config init --distro` end to end (phux-r82.9).
//!
//! Drives the real binary: scaffold a fresh config on top of the
//! bundled herdr distribution, confirm the file validates (`config
//! show` re-parses the whole stack) and that the shown effective config
//! carries distro values. Also pins the failure modes: unknown bundled
//! name, broken distro layer, and the refuse-to-overwrite contract.

#![allow(clippy::expect_used, reason = "tests")]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// Repo-checkout distros directory (the bundled-name fallback).
fn repo_distros_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("distros")
        .canonicalize()
        .expect("repo distros/ exists")
}

/// Run phux with an isolated `XDG_CONFIG_HOME` and a pinned
/// `PHUX_DISTROS_DIR` so bundled-name resolution is hermetic.
fn run(args: &[&str], xdg_config_home: &Path, distros_dir: &Path) -> (i32, String, String) {
    let out = Command::new(PHUX)
        .env("XDG_CONFIG_HOME", xdg_config_home)
        .env("PHUX_DISTROS_DIR", distros_dir)
        .args(args)
        .output()
        .expect("run phux binary");
    (
        out.status.code().expect("phux exited via code, not signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn init_distro_herdr_then_show_reflects_distro_values() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let distros = repo_distros_dir();

    // Fresh scaffold on top of the bundled herdr package (bare name).
    let (code, stdout, stderr) = run(&["config", "init", "--distro", "herdr"], &xdg, &distros);
    assert_eq!(code, 0, "init --distro herdr must succeed; stderr={stderr}");
    let config_path = xdg.join("phux").join("config.toml");
    assert!(
        stdout.contains(&config_path.display().to_string()),
        "init reports the written path; stdout={stdout}"
    );
    let written = std::fs::read_to_string(&config_path).expect("scaffold written");
    assert!(
        written.contains("extends = ["),
        "scaffold carries the active extends line: {written}"
    );
    assert!(
        written.contains("herdr.toml"),
        "extends points at the herdr layer: {written}"
    );

    // Validation: `config show` re-resolves and re-merges the stack; a
    // non-zero exit here means the scaffolded config does not validate.
    let (code, shown, stderr) = run(&["config", "show"], &xdg, &distros);
    assert_eq!(
        code, 0,
        "config show must validate the stack; stderr={stderr}"
    );

    // Distro values are in effect.
    assert!(
        shown.contains("which-key-delay-ms = 400"),
        "herdr which-key delay missing from effective config:\n{shown}"
    );
    assert!(
        shown.contains(r#"session-name-template = "${cwd-basename}""#),
        "herdr session naming missing:\n{shown}"
    );
    assert!(
        shown.contains("continuum/phux-plugin.toml")
            && shown.contains("agent-tools/phux-plugin.toml"),
        "herdr plugin set missing:\n{shown}"
    );
    // Untouched keys still track the shipped defaults.
    assert!(
        shown.contains(r#"prefix = "C-a""#),
        "shipped default prefix missing:\n{shown}"
    );

    // The wired plugin manifests actually load through the plugin path.
    let (code, plugins, stderr) = run(&["config", "plugins"], &xdg, &distros);
    assert_eq!(
        code, 0,
        "config plugins must load manifests; stderr={stderr}"
    );
    assert!(
        plugins.contains("com.phux.demo.continuum")
            && plugins.contains("com.phux.demo.agent-tools"),
        "distro-wired plugins must enumerate: {plugins}"
    );
}

#[test]
fn init_distro_accepts_a_path_spec() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let herdr_path = repo_distros_dir().join("herdr").join("herdr.toml");
    // Empty distros dir: the path spec must not consult it.
    let empty = tmp.path().join("empty-distros");
    std::fs::create_dir_all(&empty).expect("mkdir");

    let (code, _, stderr) = run(
        &[
            "config",
            "init",
            "--distro",
            herdr_path.to_str().expect("utf8"),
        ],
        &xdg,
        &empty,
    );
    assert_eq!(code, 0, "path spec must resolve; stderr={stderr}");
    let (code, _, stderr) = run(&["config", "show"], &xdg, &empty);
    assert_eq!(code, 0, "path-spec scaffold must validate; stderr={stderr}");
}

#[test]
fn init_distro_unknown_name_fails_and_writes_nothing() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let empty = tmp.path().join("empty-distros");
    std::fs::create_dir_all(&empty).expect("mkdir");

    let (code, _, stderr) = run(&["config", "init", "--distro", "nope"], &xdg, &empty);
    assert_ne!(code, 0, "unknown distro name must fail");
    assert!(
        stderr.contains("unknown distro") && stderr.contains("nope"),
        "stderr names the spec: {stderr}"
    );
    assert!(
        !xdg.join("phux").join("config.toml").exists(),
        "a failed init must not leave a config behind"
    );
}

#[test]
fn init_distro_broken_layer_fails_before_writing() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let distros = tmp.path().join("distros");
    let package = distros.join("broken");
    std::fs::create_dir_all(&package).expect("mkdir");
    std::fs::write(package.join("broken.toml"), "not = valid = toml\n").expect("write layer");

    let (code, _, stderr) = run(&["config", "init", "--distro", "broken"], &xdg, &distros);
    assert_ne!(code, 0, "broken distro layer must fail init");
    assert!(
        stderr.contains("broken.toml"),
        "stderr names the offending layer: {stderr}"
    );
    assert!(
        !xdg.join("phux").join("config.toml").exists(),
        "validation failure must not leave a config behind"
    );
}

#[test]
fn init_distro_refuses_to_overwrite_without_force() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let distros = repo_distros_dir();

    let (code, _, _) = run(&["config", "init", "--distro", "herdr"], &xdg, &distros);
    assert_eq!(code, 0);
    let config_path = xdg.join("phux").join("config.toml");
    std::fs::write(&config_path, "# user edits\n").expect("simulate user edits");

    let (code, _, stderr) = run(&["config", "init", "--distro", "herdr"], &xdg, &distros);
    assert_ne!(code, 0, "second init must refuse without --force");
    assert!(
        stderr.contains("--force"),
        "stderr suggests --force: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&config_path).expect("read back"),
        "# user edits\n",
        "refused init must not clobber"
    );

    let (code, _, stderr) = run(
        &["config", "init", "--distro", "herdr", "--force"],
        &xdg,
        &distros,
    );
    assert_eq!(code, 0, "forced init must overwrite; stderr={stderr}");
    assert!(
        std::fs::read_to_string(&config_path)
            .expect("read back")
            .contains("extends = ["),
        "forced init rewrites the distro scaffold"
    );
}
