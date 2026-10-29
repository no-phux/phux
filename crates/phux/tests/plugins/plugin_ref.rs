//! GitHub shorthand precedence, subdirectory installs from a local git
//! repository, and `phux plugin new`. No test contacts the network.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

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

    fn run_in(&self, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        let out = crate::common::phux_cmd(crate::runner::phux_bin())
            .env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("XDG_DATA_HOME", &self.xdg_data)
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(cwd)
            .args(args)
            .output()
            .expect("run phux binary");
        (
            out.status.code().expect("phux exited via code, not signal"),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        self.run_in(&self.scratch, args)
    }

    fn plugins_dir(&self) -> PathBuf {
        self.xdg_data.join("phux").join("plugins")
    }

    fn lockfile(&self) -> PathBuf {
        self.plugins_dir().join("plugins.lock")
    }
}

fn write_plugin(dir: &Path, id: &str, version: &str) {
    std::fs::create_dir_all(dir).expect("create plugin dir");
    std::fs::write(
        dir.join("phux-plugin.toml"),
        format!(
            r#"
id = "{id}"
name = "Install Fixture"
version = "{version}"
min_phux_version = "0.0.1"

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

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
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
}

#[test]
fn install_help_states_local_path_precedence() {
    let home = TestHome::new();
    let (code, stdout, stderr) = home.run(&["plugin", "install", "--help"]);
    assert_eq!(code, 0, "help should exit 0; stderr={stderr}");
    assert!(stdout.contains("owner/repo"), "{stdout}");
    assert!(
        stdout.contains("existing local path") || stdout.contains("existing local directory"),
        "{stdout}"
    );
    assert!(stdout.contains("chosen"), "{stdout}");
    assert!(stdout.contains("--subdir"), "{stdout}");
    assert!(stdout.contains("--rev"), "{stdout}");
}

#[test]
fn existing_directory_wins_over_github_shorthand() {
    let home = TestHome::new();
    let plugin = home.scratch.join("acme").join("tools");
    write_plugin(&plugin, "example.local-wins", "0.1.0");

    let (code, stdout, stderr) = home.run(&["plugin", "install", "acme/tools"]);

    assert_eq!(code, 0, "local path should win; stderr={stderr}");
    assert!(stdout.contains("installed example.local-wins"), "{stdout}");
    assert!(
        stderr.contains("chosen as a local directory"),
        "stderr should say the local path was chosen: {stderr}"
    );
    assert!(stderr.contains("https://github.com/acme/tools"), "{stderr}");
    assert!(stderr.contains("was not used"), "{stderr}");
    let lock = std::fs::read_to_string(home.lockfile()).expect("read lockfile");
    assert!(lock.contains(r#"source = "dir""#), "{lock}");
    assert!(!lock.contains("github.com"), "{lock}");
}

#[test]
fn install_git_subdir_records_rev_and_updates() {
    let home = TestHome::new();
    let repo = home.scratch.join("repo");
    write_plugin(
        &repo.join("plugins").join("demo"),
        "example.subdir",
        "0.1.0",
    );
    std::fs::write(repo.join("README.md"), "not a plugin\n").unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "fixture"]);
    git(&repo, &["checkout", "--quiet", "-b", "release"]);
    write_plugin(
        &repo.join("plugins").join("demo"),
        "example.subdir-rev",
        "0.2.0",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "release"]);
    git(&repo, &["checkout", "--quiet", "main"]);
    let url = format!("file://{}", repo.display());

    let (code, stdout, stderr) = home.run(&["plugin", "install", &url, "--json"]);
    assert_ne!(code, 0, "a repo with no root manifest must fail");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        stderr.contains("phux-plugin.toml"),
        "root miss should name the manifest: {stderr}"
    );

    let (code, stdout, stderr) = home.run(&[
        "plugin",
        "install",
        &url,
        "--subdir",
        "plugins/demo",
        "--json",
    ]);
    assert_eq!(code, 0, "subdir install should exit 0; stderr={stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("install stdout is JSON");
    assert_eq!(value["installed"]["id"], "example.subdir");
    assert_eq!(value["installed"]["version"], "0.1.0");
    assert_eq!(value["installed"]["source"], "git");
    assert_eq!(value["installed"]["ref"], url);
    assert_eq!(value["installed"]["subdir"], "plugins/demo");
    assert!(value["installed"]["note"].is_null());
    let rev = value["installed"]["rev"].as_str().expect("resolved rev");
    assert_eq!(rev.len(), 40, "{rev}");
    let installed = home.plugins_dir().join("example.subdir");
    assert!(installed.join("payload.txt").is_file());
    assert!(!installed.join(".git").exists());
    let lock = std::fs::read_to_string(home.lockfile()).unwrap();
    assert!(lock.contains(r#"subdir = "plugins/demo""#), "{lock}");

    let (code, stdout, stderr) = home.run(&[
        "plugin",
        "install",
        &url,
        "--rev",
        "release",
        "--subdir",
        "plugins/demo",
        "--json",
    ]);
    assert_eq!(code, 0, "--rev install should exit 0; stderr={stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["installed"]["id"], "example.subdir-rev");
    assert_eq!(value["installed"]["version"], "0.2.0");
    assert_eq!(value["installed"]["branch"], "release");
    assert_eq!(value["installed"]["subdir"], "plugins/demo");

    write_plugin(
        &repo.join("plugins").join("demo"),
        "example.subdir",
        "0.1.1",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "bump"]);
    let (code, stdout, stderr) = home.run(&["plugin", "update", "example.subdir", "--json"]);
    assert_eq!(
        code, 0,
        "update should re-read the subdirectory; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["updated"][0]["version"], "0.1.1");
}

#[test]
fn plugin_new_output_validates_and_links() {
    let home = TestHome::new();
    let dir = home.scratch.join("hello");

    let (code, stdout, stderr) =
        home.run(&["plugin", "new", "hello", "--dir", dir.to_str().unwrap()]);
    assert_eq!(code, 0, "plugin new should exit 0; stderr={stderr}");
    assert!(stdout.contains("created hello"), "{stdout}");
    assert!(dir.join("phux-plugin.toml").is_file());
    assert!(dir.join("README.md").is_file());
    assert!(dir.join("scripts").join("hello.sh").is_file());

    let manifest = std::fs::read_to_string(dir.join("phux-plugin.toml")).unwrap();
    assert!(manifest.contains(&format!(
        "min_phux_version = \"{}\"",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(manifest.contains("on = \"pane-exit\""));
    assert!(manifest.contains("scripts/hello.sh"));
    for relative in ["phux-plugin.toml", "README.md", "scripts/hello.sh"] {
        let text = std::fs::read_to_string(dir.join(relative)).unwrap();
        let scrubbed = text.replace("PHUX_", "");
        assert!(!scrubbed.contains("Phux"), "{relative}: {text}");
        assert!(!scrubbed.contains("PHUX"), "{relative}: {text}");
    }

    let (code, stdout, stderr) = home.run(&["plugin", "validate", dir.to_str().unwrap()]);
    assert_eq!(
        code, 0,
        "validate should accept the scaffold; stderr={stderr}"
    );
    assert!(stdout.contains("valid hello"), "{stdout}");

    let (code, stdout, stderr) = home.run(&["plugin", "link", dir.to_str().unwrap()]);
    assert_eq!(code, 0, "link should accept the scaffold; stderr={stderr}");
    assert!(stdout.contains("linked hello"), "{stdout}");
}

#[test]
fn plugin_new_refuses_non_empty_and_invalid_names() {
    let home = TestHome::new();
    let dir = home.scratch.join("hello");
    let (code, _, stderr) = home.run(&["plugin", "new", "hello", "--dir", dir.to_str().unwrap()]);
    assert_eq!(code, 0, "scaffold should exit 0; stderr={stderr}");

    let marker = dir.join("keep.txt");
    std::fs::write(&marker, "keep").unwrap();
    let (code, stdout, stderr) =
        home.run(&["plugin", "new", "hello", "--dir", dir.to_str().unwrap()]);
    assert_ne!(code, 0, "a non-empty directory must be refused");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("not empty"), "{stderr}");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "keep");

    let empty = home.scratch.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let (code, _, stderr) = home.run(&[
        "plugin",
        "new",
        "com.example.tools",
        "--dir",
        empty.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "an empty directory is writable; stderr={stderr}");
    let (code, stdout, stderr) = home.run(&["plugin", "validate", empty.to_str().unwrap()]);
    assert_eq!(
        code, 0,
        "empty-dir scaffold should validate; stderr={stderr}"
    );
    assert!(stdout.contains("valid com.example.tools"), "{stdout}");

    let (code, _, stderr) = home.run(&["plugin", "new", "Not A Plugin"]);
    assert_ne!(code, 0, "an invalid id must be refused; stderr={stderr}");
    assert!(stderr.contains("not a plugin id"), "{stderr}");
    assert!(!home.scratch.join("Not A Plugin").exists());
}
