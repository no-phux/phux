//! End-to-end coverage for `phux plugin install` / `phux plugin update`
//! (phux-r82.8) and the `min_phux_version` gate they enforce (phux-r82.2).
//!
//! Every test spawns the real binary with isolated `XDG_CONFIG_HOME` /
//! `XDG_DATA_HOME` roots, so installs land in a per-test managed plugins
//! dir and link into a per-test `config.toml`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

struct TestHome {
    _tmp: TempDir,
    xdg_config: PathBuf,
    xdg_data: PathBuf,
    scratch: PathBuf,
}

impl TestHome {
    fn new() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let xdg_config = tmp.path().join("config");
        let xdg_data = tmp.path().join("data");
        let scratch = tmp.path().join("scratch");
        std::fs::create_dir_all(&scratch).expect("create scratch dir");
        Self {
            _tmp: tmp,
            xdg_config,
            xdg_data,
            scratch,
        }
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(PHUX)
            .env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("XDG_DATA_HOME", &self.xdg_data)
            .args(args)
            .output()
            .expect("run phux binary");
        (
            out.status.code().expect("phux exited via code, not signal"),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn plugins_dir(&self) -> PathBuf {
        self.xdg_data.join("phux").join("plugins")
    }

    fn config_toml(&self) -> PathBuf {
        self.xdg_config.join("phux").join("config.toml")
    }

    fn lockfile(&self) -> PathBuf {
        self.plugins_dir().join("plugins.lock")
    }
}

fn write_fixture_plugin(dir: &Path, id: &str, version: &str, min_phux: &str, build: &str) {
    std::fs::create_dir_all(dir).expect("create plugin dir");
    std::fs::write(
        dir.join("phux-plugin.toml"),
        format!(
            r#"
id = "{id}"
name = "Install Fixture"
version = "{version}"
min_phux_version = "{min_phux}"

{build}

[[actions]]
id = "open"
title = "Open"
command = ["true"]
"#
        ),
    )
    .expect("write manifest");
    std::fs::write(dir.join("payload.txt"), "payload").expect("write payload");
}

const BUILD_OK: &str = r#"
[[build]]
command = ["sh", "-c", "echo built > build-out.txt"]
"#;

const BUILD_FAIL: &str = r#"
[[build]]
command = ["sh", "-c", "echo boom >&2; exit 3"]
"#;

/// Install from a local fixture directory: the tree is copied into the
/// managed plugins dir, the [[build]] step runs inside the managed copy,
/// the entry is linked into config.toml, and plugins.lock records the
/// source dir.
#[test]
fn install_from_local_dir_builds_links_and_locks() {
    let home = TestHome::new();
    let fixture = home.scratch.join("fixture");
    write_fixture_plugin(&fixture, "example.install-dir", "0.1.0", "0.0.1", BUILD_OK);

    let (code, stdout, stderr) = home.run(&[
        "plugin",
        "install",
        fixture.to_str().expect("utf-8 path"),
        "--json",
    ]);

    assert_eq!(code, 0, "install should exit 0; stderr={stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("install stdout is JSON");
    assert_eq!(value["installed"]["id"], "example.install-dir");
    assert_eq!(value["installed"]["source"], "dir");
    assert_eq!(value["installed"]["enabled"], true);
    assert!(value["installed"]["rev"].is_null());

    let installed = home.plugins_dir().join("example.install-dir");
    assert!(installed.join("phux-plugin.toml").is_file());
    assert!(installed.join("payload.txt").is_file());
    assert!(
        installed.join("build-out.txt").is_file(),
        "build step must run inside the managed copy"
    );

    let config = std::fs::read_to_string(home.config_toml()).expect("read config");
    assert!(
        config.contains("example.install-dir"),
        "config.toml should link the installed manifest: {config}"
    );

    let lock = std::fs::read_to_string(home.lockfile()).expect("read lockfile");
    assert!(lock.contains(r#"id = "example.install-dir""#), "{lock}");
    assert!(lock.contains(r#"source = "dir""#), "{lock}");

    let (code, stdout, stderr) = home.run(&["plugin", "list", "--json"]);
    assert_eq!(code, 0, "list should exit 0; stderr={stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("list stdout is JSON");
    assert_eq!(value["plugins"][0]["id"], "example.install-dir");
    assert_eq!(value["plugins"][0]["version"], "0.1.0");
}

/// Installing from a tarball extracts with the system tar and behaves
/// like the dir path from there on.
#[test]
fn install_from_tarball_extracts_and_links() {
    let home = TestHome::new();
    let fixture = home.scratch.join("fixture");
    write_fixture_plugin(&fixture, "example.install-tar", "0.1.0", "0.0.1", "");
    let tarball = home.scratch.join("plugin.tar.gz");
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&tarball)
        .arg("-C")
        .arg(&home.scratch)
        .arg("fixture")
        .status()
        .expect("run tar");
    assert!(status.success(), "tar should create the fixture tarball");

    let (code, stdout, stderr) = home.run(&[
        "plugin",
        "install",
        tarball.to_str().expect("utf-8 path"),
        "--json",
    ]);

    assert_eq!(code, 0, "tarball install should exit 0; stderr={stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("install stdout is JSON");
    assert_eq!(value["installed"]["id"], "example.install-tar");
    assert_eq!(value["installed"]["source"], "tarball");
    assert!(
        home.plugins_dir()
            .join("example.install-tar")
            .join("payload.txt")
            .is_file()
    );
}

/// Installing from a git URL clones with the system git and records the
/// resolved commit in plugins.lock.
#[test]
fn install_from_git_url_records_resolved_commit() {
    let home = TestHome::new();
    let repo = home.scratch.join("repo");
    write_fixture_plugin(&repo, "example.install-git", "0.1.0", "0.0.1", "");
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=Test",
            ])
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} should succeed");
    };
    git(&["init", "--quiet", "--initial-branch=main"]);
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "fixture"]);
    let url = format!("file://{}", repo.display());

    let (code, stdout, stderr) = home.run(&["plugin", "install", &url, "--json"]);

    assert_eq!(code, 0, "git install should exit 0; stderr={stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("install stdout is JSON");
    assert_eq!(value["installed"]["id"], "example.install-git");
    assert_eq!(value["installed"]["source"], "git");
    let rev = value["installed"]["rev"].as_str().expect("resolved rev");
    assert_eq!(rev.len(), 40, "rev should be a full commit hash: {rev}");
    assert!(rev.bytes().all(|b| b.is_ascii_hexdigit()));

    let lock = std::fs::read_to_string(home.lockfile()).expect("read lockfile");
    assert!(lock.contains(&format!(r#"rev = "{rev}""#)), "{lock}");
    // The managed copy is a snapshot, not a working clone (ADR-0041).
    assert!(
        !home
            .plugins_dir()
            .join("example.install-git")
            .join(".git")
            .exists(),
        "managed git install must not retain .git"
    );
}

/// The `min_phux_version` gate (phux-r82.2) refuses an install whose floor
/// is newer than this phux, naming both versions, and leaves no trace:
/// no managed dir, no lockfile entry, no config link.
#[test]
fn install_rejects_future_min_phux_version() {
    let home = TestHome::new();
    let fixture = home.scratch.join("fixture");
    write_fixture_plugin(&fixture, "example.install-future", "0.1.0", "99.0.0", "");

    let (code, stdout, stderr) = home.run(&[
        "plugin",
        "install",
        fixture.to_str().expect("utf-8 path"),
        "--json",
    ]);

    assert_ne!(code, 0, "future floor must fail the install");
    assert!(stdout.is_empty(), "no JSON on failure: {stdout}");
    assert!(stderr.contains("requires phux >= 99.0.0"), "{stderr}");
    assert!(
        stderr.contains(env!("CARGO_PKG_VERSION")),
        "error must name the running phux version: {stderr}"
    );
    assert!(!home.plugins_dir().join("example.install-future").exists());
    assert!(!home.lockfile().exists());
    let config = std::fs::read_to_string(home.config_toml()).unwrap_or_default();
    assert!(!config.contains("example.install-future"), "{config}");
}

/// A failing [[build]] step surfaces its exit code and captured stderr,
/// and nothing is linked or locked.
#[test]
fn install_build_failure_surfaces_and_does_not_link() {
    let home = TestHome::new();
    let fixture = home.scratch.join("fixture");
    write_fixture_plugin(
        &fixture,
        "example.install-broken",
        "0.1.0",
        "0.0.1",
        BUILD_FAIL,
    );

    let (code, stdout, stderr) =
        home.run(&["plugin", "install", fixture.to_str().expect("utf-8 path")]);

    assert_ne!(code, 0, "failing build must fail the install");
    assert!(stdout.is_empty(), "no success output: {stdout}");
    assert!(stderr.contains("build step 1"), "{stderr}");
    assert!(stderr.contains("exit code Some(3)"), "{stderr}");
    assert!(
        stderr.contains("boom"),
        "captured stderr surfaces: {stderr}"
    );
    assert!(!home.plugins_dir().join("example.install-broken").exists());
    assert!(!home.lockfile().exists());
    let config = std::fs::read_to_string(home.config_toml()).unwrap_or_default();
    assert!(!config.contains("example.install-broken"), "{config}");
    // No staging debris left behind.
    if let Ok(entries) = std::fs::read_dir(home.plugins_dir()) {
        for entry in entries {
            let name = entry.expect("dir entry").file_name();
            assert!(
                !name.to_string_lossy().starts_with(".staging-"),
                "staging dir {name:?} should have been cleaned up"
            );
        }
    }
}

/// `phux plugin update NAME` re-fetches from the recorded source, reruns
/// the build, and swaps the managed copy — picking up a new version.
#[test]
fn update_refetches_from_recorded_source() {
    let home = TestHome::new();
    let fixture = home.scratch.join("fixture");
    write_fixture_plugin(&fixture, "example.install-upd", "0.1.0", "0.0.1", BUILD_OK);
    let (code, _, stderr) = home.run(&["plugin", "install", fixture.to_str().expect("utf-8 path")]);
    assert_eq!(code, 0, "install should exit 0; stderr={stderr}");

    // The upstream source moves forward.
    write_fixture_plugin(&fixture, "example.install-upd", "0.2.0", "0.0.1", BUILD_OK);

    let (code, stdout, stderr) = home.run(&["plugin", "update", "example.install-upd", "--json"]);

    assert_eq!(code, 0, "update should exit 0; stderr={stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("update stdout is JSON");
    assert_eq!(value["updated"][0]["id"], "example.install-upd");
    assert_eq!(value["updated"][0]["version"], "0.2.0");

    let (code, stdout, stderr) = home.run(&["plugin", "list", "--json"]);
    assert_eq!(code, 0, "list should exit 0; stderr={stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("list stdout is JSON");
    assert_eq!(
        value["plugins"][0]["version"], "0.2.0",
        "linked manifest should now be the updated copy"
    );
}

/// Updating a name that was never installed is a clear, named error.
#[test]
fn update_unknown_plugin_is_a_named_error() {
    let home = TestHome::new();

    let (code, stdout, stderr) = home.run(&["plugin", "update", "example.ghost"]);

    assert_ne!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.contains("example.ghost"), "{stderr}");
    assert!(stderr.contains("plugins.lock"), "{stderr}");
}

/// Reinstalling an already-installed plugin points at update instead of
/// silently clobbering the managed copy.
#[test]
fn reinstall_is_refused_in_favor_of_update() {
    let home = TestHome::new();
    let fixture = home.scratch.join("fixture");
    write_fixture_plugin(&fixture, "example.install-twice", "0.1.0", "0.0.1", "");
    let (code, _, stderr) = home.run(&["plugin", "install", fixture.to_str().expect("utf-8 path")]);
    assert_eq!(code, 0, "first install should exit 0; stderr={stderr}");

    let (code, stdout, stderr) =
        home.run(&["plugin", "install", fixture.to_str().expect("utf-8 path")]);

    assert_ne!(code, 0, "second install must be refused");
    assert!(stdout.is_empty());
    assert!(stderr.contains("already installed"), "{stderr}");
    assert!(
        stderr.contains("phux plugin update example.install-twice"),
        "{stderr}"
    );
}
