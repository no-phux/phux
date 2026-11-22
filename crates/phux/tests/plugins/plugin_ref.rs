//! GitHub shorthand precedence, subdirectory installs from a local git
//! repository, and `phux plugin new`. No test contacts the network.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use crate::support::{TestHome, commit_all, git, init_repo, write_fixture_plugin};

#[test]
fn install_git_subdir_records_rev_and_updates() {
    let home = TestHome::new();
    let repo = home.scratch.join("repo");
    write_fixture_plugin(
        &repo.join("plugins").join("demo"),
        "example.subdir",
        "0.1.0",
        "0.0.1",
        "",
    );
    std::fs::write(repo.join("README.md"), "not a plugin\n").unwrap();
    let url = init_repo(&repo, "fixture");
    git(&repo, &["checkout", "--quiet", "-b", "release"]);
    write_fixture_plugin(
        &repo.join("plugins").join("demo"),
        "example.subdir-rev",
        "0.2.0",
        "0.0.1",
        "",
    );
    commit_all(&repo, "release");
    git(&repo, &["checkout", "--quiet", "main"]);

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

    write_fixture_plugin(
        &repo.join("plugins").join("demo"),
        "example.subdir",
        "0.1.1",
        "0.0.1",
        "",
    );
    commit_all(&repo, "bump");
    let (code, stdout, stderr) = home.run(&["plugin", "update", "example.subdir", "--json"]);
    assert_eq!(
        code, 0,
        "update should re-read the subdirectory; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["updated"][0]["version"], "0.1.1");
}
