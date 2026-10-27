//! The local patch against real `git`: a "GitHub" bare repository holding a
//! `refs/pull/N/head`, and a clone that has not fetched it.

use std::path::PathBuf;

use super::*;

fn sh(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=T",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn fresh(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("phux-github-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A remote whose `main` has one file and whose pull request 7 edits line 30
/// of 60; then `main` moves on so the merge base is not the base tip.
fn hosted(name: &str) -> (PathBuf, String, String) {
    let root = fresh(name);
    let work = root.join("work");
    let hub = root.join("hub.git");
    std::fs::create_dir_all(&work).unwrap();
    sh(&work, &["init", "-q", "-b", "main"]);
    let lines = (1..=60).fold(String::new(), |mut lines, n| {
        use std::fmt::Write as _;
        let _ = writeln!(lines, "line {n}");
        lines
    });
    std::fs::write(work.join("a.txt"), &lines).unwrap();
    sh(&work, &["add", "."]);
    sh(&work, &["commit", "-qm", "base"]);
    sh(&work, &["checkout", "-qb", "feat"]);
    std::fs::write(
        work.join("a.txt"),
        lines.replace("line 30\n", "line thirty\n"),
    )
    .unwrap();
    sh(&work, &["commit", "-qam", "edit"]);
    let head = sh(&work, &["rev-parse", "HEAD"]);
    sh(&work, &["checkout", "-q", "main"]);
    std::fs::write(work.join("b.txt"), "other\n").unwrap();
    sh(&work, &["add", "."]);
    sh(&work, &["commit", "-qm", "later"]);
    let base = sh(&work, &["rev-parse", "HEAD"]);
    sh(&root, &["init", "-q", "--bare", "hub.git"]);
    let url = hub.to_string_lossy().into_owned();
    sh(
        &work,
        &["push", "-q", &url, "main", "feat:refs/pull/7/head"],
    );
    (root, base, head)
}

#[test]
fn a_clone_without_the_head_fetches_it_and_keeps_every_line_of_context() {
    let (root, base, head) = hosted("fetch");
    let clone = root.join("clone");
    sh(
        &root,
        &[
            "clone",
            "-q",
            "--no-local",
            "--single-branch",
            "-b",
            "main",
            "hub.git",
            "clone",
        ],
    );
    assert!(!has_commit(&clone, &head));
    let refs_before = sh(&clone, &["for-each-ref"]);

    let range = Range {
        number: 7,
        base_ref: "main",
        base_oid: &base,
        head_oid: &head,
    };
    let patch = local(&clone, "origin", &range, 100_000).unwrap();

    assert!(!patch.reduced);
    // Only a.txt: b.txt landed on main after the merge base.
    assert_eq!(crate::split_files(&patch.text).len(), 1);
    assert!(patch.text.contains("-line 30\n+line thirty\n"));
    assert!(patch.text.contains(" line 1\n") && patch.text.contains(" line 60\n"));
    assert_eq!(
        sh(&clone, &["for-each-ref"]),
        refs_before,
        "no ref was written"
    );
    assert!(!clone.join(".git/FETCH_HEAD").exists());
}

#[test]
fn a_head_the_remote_lacks_is_an_error() {
    let (root, base, _) = hosted("missing");
    let clone = root.join("clone");
    sh(&root, &["clone", "-q", "--no-local", "hub.git", "clone"]);
    let missing = "0123456789012345678901234567890123456789";
    let range = Range {
        number: 7,
        base_ref: "main",
        base_oid: &base,
        head_oid: missing,
    };
    assert!(local(&clone, "origin", &range, 3).is_err());
}

#[test]
fn the_matching_remote_is_found_by_url() {
    let (root, _, _) = hosted("remote");
    let clone = root.join("clone");
    sh(&root, &["clone", "-q", "--no-local", "hub.git", "clone"]);
    sh(
        &clone,
        &[
            "remote",
            "add",
            "upstream",
            "git@github.com:Acme/Widget.git",
        ],
    );
    assert_eq!(
        remote_for(&clone, "acme/widget").as_deref(),
        Some("upstream")
    );
    sh(
        &clone,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/widget",
        ],
    );
    assert_eq!(remote_for(&clone, "acme/widget").as_deref(), Some("origin"));
    assert_eq!(remote_for(&clone, "acme/other"), None);
}

#[test]
fn remote_urls_name_their_repository() {
    for url in [
        "git@github.com:no-phux/phux.git",
        "https://github.com/no-phux/phux",
        "https://github.com/no-phux/phux.git/",
        "ssh://git@github.com/no-phux/phux.git",
        "ssh://git@ghe.example.com:2222/no-phux/phux",
    ] {
        assert_eq!(repo_of_url(url).as_deref(), Some("no-phux/phux"), "{url}");
    }
    assert_eq!(repo_of_url("/local/path"), None);
}
