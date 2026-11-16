//! Fixtures shared by the plugin suites: a private config/data home, a
//! deterministic `git`, the checked-in demo plugin, and a fixture manifest.

#![allow(
    unreachable_pub,
    reason = "shared by sibling suites in this test binary"
)]
#![allow(clippy::expect_used, reason = "tests")]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use crate::common::Out;

/// The checked-in demo plugin's id.
pub const PLUGIN_ID: &str = "com.phux.demo.agent-tools";

/// The repository root, canonicalized.
pub fn repo_root() -> PathBuf {
    crate::runner::manifest_dir()
        .join("../..")
        .canonicalize()
        .expect("canonical repo root")
}

/// The `XDG_CONFIG_HOME` that enables the checked-in demo plugin.
pub fn demo_xdg() -> PathBuf {
    repo_root().join("plugins/agent-tools/config")
}

/// A private `XDG_CONFIG_HOME`/`XDG_DATA_HOME` and a scratch directory that
/// every `phux` here runs in.
pub struct TestHome {
    _tmp: TempDir,
    pub xdg_config: PathBuf,
    pub xdg_data: PathBuf,
    pub scratch: PathBuf,
}

impl TestHome {
    pub fn new() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let scratch = tmp.path().join("scratch");
        std::fs::create_dir_all(&scratch).expect("create scratch dir");
        Self {
            xdg_config: tmp.path().join("config"),
            xdg_data: tmp.path().join("data"),
            scratch,
            _tmp: tmp,
        }
    }

    /// `phux <args...>` in the scratch directory.
    pub fn run(&self, args: &[&str]) -> (i32, String, String) {
        self.run_with(args, |_| {})
    }

    /// [`Self::run`] with `edit` applied to the command last.
    pub fn run_with(
        &self,
        args: &[&str],
        edit: impl FnOnce(&mut Command),
    ) -> (i32, String, String) {
        let mut cmd = crate::common::phux_cmd(crate::runner::phux_bin());
        cmd.env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("XDG_DATA_HOME", &self.xdg_data)
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(&self.scratch)
            .args(args);
        edit(&mut cmd);
        Out::of(&mut cmd).into_parts()
    }

    pub fn plugins_dir(&self) -> PathBuf {
        self.xdg_data.join("phux").join("plugins")
    }

    pub fn config_toml(&self) -> PathBuf {
        self.xdg_config.join("phux").join("config.toml")
    }

    pub fn lockfile(&self) -> PathBuf {
        self.plugins_dir().join("plugins.lock")
    }
}

/// `git -C repo <args...>` with a fixed identity; must succeed.
pub fn git(repo: &Path, args: &[&str]) {
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

/// Stage everything in `repo` and commit it as `message`.
pub fn commit_all(repo: &Path, message: &str) {
    git(repo, &["add", "."]);
    git(repo, &["commit", "--quiet", "-m", message]);
}

/// `git init` `repo` on `main`, commit its contents as `message`, and return
/// its `file://` URL.
pub fn init_repo(repo: &Path, message: &str) -> String {
    git(repo, &["init", "--quiet", "--initial-branch=main"]);
    commit_all(repo, message);
    format!("file://{}", repo.display())
}

/// A minimal installable plugin in `dir`: a manifest with one action, any
/// `build` table, and a payload file.
pub fn write_fixture_plugin(
    dir: &Path,
    id: &str,
    version: &str,
    min_phux: &str,
    build: &str,
) {
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
