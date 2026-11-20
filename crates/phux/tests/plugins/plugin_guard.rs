//! Guards on `phux plugin install` / `update`: git is not handed options,
//! fetches time out, symlinks are refused, and recorded commits are pinned.
//! No test contacts the network.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::path::Path;
use std::process::Command;

use crate::support::{TestHome, commit_all, git, init_repo};

fn write_plugin(dir: &Path, id: &str, version: &str, build: bool) {
    std::fs::create_dir_all(dir).unwrap();
    let build = if build {
        "\n[[build]]\ncommand = [\"sh\", \"-c\", \"echo built > build-out.txt\"]\n"
    } else {
        ""
    };
    std::fs::write(
        dir.join("phux-plugin.toml"),
        format!(
            "id = \"{id}\"\nname = \"Guard\"\nversion = \"{version}\"\n\
             min_phux_version = \"0.0.1\"\n{build}\n\
             [[actions]]\nid = \"open\"\ntitle = \"Open\"\ncommand = [\"true\"]\n"
        ),
    )
    .unwrap();
}

/// The installed copy of plugin `id`'s manifest.
fn installed_manifest(home: &TestHome, id: &str) -> String {
    std::fs::read_to_string(home.plugins_dir().join(id).join("phux-plugin.toml")).unwrap()
}

fn head(repo: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn cloned_symlink_is_refused() {
    let home = TestHome::new();
    let repo = home.scratch.join("repo");
    write_plugin(&repo, "example.symlink", "0.1.0", false);
    std::os::unix::fs::symlink("/etc/passwd", repo.join("leak")).unwrap();
    let url = init_repo(&repo, "link");

    let (code, stdout, stderr) = home.run(&["plugin", "install", &url]);
    assert_ne!(code, 0, "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("is a symlink"), "{stderr}");
    assert!(!home.plugins_dir().join("example.symlink").exists());
}

#[test]
fn remote_build_requires_yes_off_a_terminal() {
    let home = TestHome::new();
    let repo = home.scratch.join("repo");
    write_plugin(&repo, "example.remote-build", "0.1.0", true);
    let url = init_repo(&repo, "build");

    let (code, _, stderr) = home.run(&["plugin", "install", &url]);
    assert_ne!(code, 0, "{stderr}");
    assert!(stderr.contains("--yes"), "{stderr}");
    assert!(stderr.contains("Nothing was installed"), "{stderr}");
    assert!(stderr.contains("Refused:"), "{stderr}");
    assert!(stderr.contains("echo built"), "{stderr}");
    assert!(!home.plugins_dir().join("example.remote-build").exists());

    let (code, _, stderr) = home.run(&["plugin", "install", &url, "--yes"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        home.plugins_dir()
            .join("example.remote-build")
            .join("build-out.txt")
            .is_file()
    );
}

fn plant_stale_build_hash(home: &TestHome) {
    let path = home.plugins_dir().join("plugins.lock");
    let lock = std::fs::read_to_string(&path).unwrap();
    assert!(lock.contains("build_steps"), "{lock}");
    let stale = lock
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("build_steps") {
                "build_steps = \"legacy-encoding\""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(path, stale).unwrap();
}

#[test]
fn rev_pins_a_commit_until_latest() {
    let home = TestHome::new();
    let repo = home.scratch.join("repo");
    write_plugin(&repo, "example.pin", "0.1.0", false);
    let url = init_repo(&repo, "v1");
    let pinned = head(&repo);
    write_plugin(&repo, "example.pin", "0.2.0", false);
    commit_all(&repo, "v2");

    let (code, stdout, stderr) = home.run(&["plugin", "install", &url, "--rev", &pinned, "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["installed"]["version"], "0.1.0");
    assert_eq!(value["installed"]["rev"], pinned);

    let short = &pinned[..7];
    let (code, _, stderr) = home.run(&["plugin", "install", &url, "--rev", short]);
    assert_ne!(code, 0, "short SHA fetch should say so; stderr={stderr}");
    assert!(stderr.contains("full 40-character SHA"), "{stderr}");

    let (code, stdout, stderr) = home.run(&["plugin", "update", "example.pin", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["updated"][0]["rev"], pinned);
    let installed = installed_manifest(&home, "example.pin");
    assert!(installed.contains("version = \"0.1.0\""), "{installed}");

    let (code, stdout, stderr) =
        home.run(&["plugin", "update", "example.pin", "--latest", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_ne!(value["updated"][0]["rev"], pinned);
    let installed = installed_manifest(&home, "example.pin");
    assert!(installed.contains("version = \"0.2.0\""), "{installed}");
}

#[test]
fn update_refuses_a_rewritten_branch_without_latest() {
    let home = TestHome::new();
    let repo = home.scratch.join("repo");
    write_plugin(&repo, "example.ff", "0.1.0", false);
    let url = init_repo(&repo, "v1");
    let first = head(&repo);
    let (code, _, stderr) = home.run(&["plugin", "install", &url]);
    assert_eq!(code, 0, "{stderr}");

    write_plugin(&repo, "example.ff", "0.2.0", false);
    commit_all(&repo, "v2");
    let (code, stdout, stderr) = home.run(&["plugin", "update", "example.ff", "--json"]);
    assert_eq!(
        code, 0,
        "fast-forward update should succeed; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let recorded = value["updated"][0]["rev"].as_str().unwrap().to_owned();
    assert_ne!(recorded, first);

    git(&repo, &["reset", "--quiet", "--hard", &first]);
    write_plugin(&repo, "example.ff", "0.3.0", false);
    commit_all(&repo, "rewrite");
    let (code, _, stderr) = home.run(&["plugin", "update", "example.ff"]);
    assert_ne!(code, 0, "{stderr}");
    assert!(stderr.contains("not a descendant"), "{stderr}");
    assert!(stderr.contains("--latest"), "{stderr}");
    let installed = installed_manifest(&home, "example.ff");
    assert!(installed.contains("version = \"0.2.0\""), "{installed}");

    let (code, _, stderr) = home.run(&["plugin", "update", "example.ff", "--latest"]);
    assert_eq!(code, 0, "{stderr}");
    let installed = installed_manifest(&home, "example.ff");
    assert!(installed.contains("version = \"0.3.0\""), "{installed}");
}

fn manifest_with_build(dir: &Path, id: &str, version: &str, script: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("phux-plugin.toml"),
        format!(
            "id = \"{id}\"\nname = \"Guard\"\nversion = \"{version}\"\n\
             min_phux_version = \"0.0.1\"\n\n\
             [[build]]\ncommand = [\"sh\", \"-c\", \"{script}\"]\n\n\
             [[actions]]\nid = \"open\"\ntitle = \"Open\"\ncommand = [\"true\"]\n"
        ),
    )
    .unwrap();
}

#[test]
fn update_changed_build_steps_need_yes_off_a_terminal() {
    let home = TestHome::new();
    let repo = home.scratch.join("repo");
    manifest_with_build(&repo, "evil", "0.1.0", "echo A > approved.txt");
    let url = init_repo(&repo, "step A");

    let (code, _, stderr) = home.run(&["plugin", "install", &url, "--yes"]);
    assert_eq!(code, 0, "{stderr}");
    let installed = home.plugins_dir().join("evil");
    assert!(installed.join("approved.txt").is_file());

    manifest_with_build(&repo, "evil", "0.2.0", "echo B > slipped.txt");
    commit_all(&repo, "step B");
    let (code, _, stderr) = home.run(&["plugin", "update", "evil"]);
    assert_ne!(code, 0, "{stderr}");
    assert!(stderr.contains("--yes"), "{stderr}");
    assert!(stderr.contains("Nothing was updated"), "{stderr}");
    assert!(stderr.contains("Refused:"), "{stderr}");
    assert!(stderr.contains("echo B"), "{stderr}");
    assert!(installed.join("approved.txt").is_file());
    assert!(
        !installed.join("slipped.txt").exists(),
        "changed build step B must not run"
    );

    let (code, _, stderr) = home.run(&["plugin", "update", "evil", "--yes"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(installed.join("slipped.txt").is_file());

    manifest_with_build(&repo, "evil", "0.2.1", "echo B > slipped.txt");
    commit_all(&repo, "same steps");
    let (code, _, stderr) = home.run(&["plugin", "update", "evil"]);
    assert_eq!(
        code, 0,
        "unchanged build steps should not ask again; stderr={stderr}"
    );
}

#[test]
fn update_reasks_when_the_stored_build_hash_does_not_match() {
    let home = TestHome::new();
    let repo = home.scratch.join("repo");
    let marker = home.scratch.join("runs.txt");
    let script = format!("echo ran >> {}", marker.display());
    manifest_with_build(&repo, "stale", "0.1.0", &script);
    let url = init_repo(&repo, "build");

    let (code, _, stderr) = home.run(&["plugin", "install", &url, "--yes"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);

    // Any other digest, including an older encoding, takes this re-ask path.
    plant_stale_build_hash(&home);
    let (code, _, stderr) = home.run(&["plugin", "update", "stale"]);
    assert_ne!(code, 0, "{stderr}");
    assert!(stderr.contains("--yes"), "{stderr}");
    assert!(stderr.contains("Nothing was updated"), "{stderr}");
    assert!(stderr.contains("Refused:"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().lines().count(),
        1,
        "a stale build hash must not run the command"
    );
}
