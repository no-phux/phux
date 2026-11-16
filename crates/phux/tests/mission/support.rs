//! Repository, plugin, and invocation fixtures shared by the mission suites.

#![allow(unreachable_pub, reason = "shared by sibling suite modules")]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::common::ServerGuard;

pub fn init_repo(repo: &Path, origin: &Path) {
    git(repo, &["init", "-b", "main"]);
    git(repo, &["config", "user.email", "t@example.com"]);
    git(repo, &["config", "user.name", "test"]);
    git(repo, &["commit", "--allow-empty", "-m", "init"]);
    git(repo, &["init", "--bare", origin.to_str().unwrap()]);
    git(repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(repo, &["push", "-u", "origin", "main"]);
    git(repo, &["remote", "set-head", "origin", "main"]);
}

/// Run git in `dir` with a fixed identity and no user or system config.
pub fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A plugin whose `fake` integration runs `script`, which logs its
/// environment and argv to `log_dir/launch-PID.txt`, then runs `body`.
pub fn write_plugin(root: &Path, script: &Path, log_dir: &Path, body: &str) -> PathBuf {
    let plugin = root.join("plugin");
    std::fs::create_dir_all(plugin.join("integrations")).unwrap();
    let manifest = plugin.join("phux-plugin.toml");
    std::fs::write(
        &manifest,
        "id = \"com.phux.test.mission-fake\"\nname = \"mission fake\"\nversion = \"0.0.1\"\nmin_phux_version = \"0.0.1\"\ndescription = \"test\"\nplatforms = [\"linux\"]\n",
    )
    .unwrap();
    std::fs::write(
        plugin.join("integrations/fake.toml"),
        format!(
            "schema_version = 1\nid = \"fake\"\ndisplay_name = \"Fake\"\nkind = \"terminal-agent\"\n\n[launch]\ncommand = [\"/bin/sh\", \"{}\"]\nworking_directory = \"workspace\"\n",
            script.display()
        ),
    )
    .unwrap();
    let log = log_dir.display();
    std::fs::write(
        script,
        format!(
            "#!/bin/sh\nmkdir -p \"{log}\"\n{{\n  echo \"CARGO_TARGET_DIR=$CARGO_TARGET_DIR\"\n  echo \"PHUX_SOCKET=$PHUX_SOCKET\"\n  echo \"ARGS:$*\"\n}} > \"{log}/launch-$$.txt\"\n{body}"
        ),
    )
    .unwrap();
    manifest
}

/// `phux ARGS` against `guard` from `repo`, with `env` added.
pub fn mission(
    guard: &ServerGuard,
    repo: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> (i32, String, String) {
    let mut cmd = guard.cmd(args);
    cmd.current_dir(repo).envs(env.iter().copied());
    let output = cmd.output().expect("phux");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

pub fn assert_valid(value: &Value) {
    crate::golden::assert_valid(&crate::golden::schema_validator(), value);
}
