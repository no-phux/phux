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
    sh(&root, &["init", "-q", "--bare", "-b", "main", "hub.git"]);
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
fn local_patch_paths_ignore_user_diff_prefix_configuration() {
    let root = fresh("prefixes");
    sh(&root, &["init", "-q", "-b", "main"]);
    for path in ["a/nested.txt", "b/nested.txt"] {
        std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
        std::fs::write(root.join(path), "before\n").unwrap();
    }
    sh(&root, &["add", "."]);
    sh(&root, &["commit", "-qm", "base"]);
    let base = sh(&root, &["rev-parse", "HEAD"]);
    for path in ["a/nested.txt", "b/nested.txt"] {
        std::fs::write(root.join(path), "after\n").unwrap();
    }
    sh(&root, &["commit", "-qam", "head"]);
    let head = sh(&root, &["rev-parse", "HEAD"]);
    let range = Range {
        number: 7,
        base_ref: "main",
        base_oid: &base,
        head_oid: &head,
    };
    for (no_prefix, mnemonic) in [("true", "false"), ("false", "true"), ("true", "true")] {
        sh(&root, &["config", "diff.noprefix", no_prefix]);
        sh(&root, &["config", "diff.mnemonicprefix", mnemonic]);
        let patch = local(&root, "unused", &range, 3).unwrap();
        assert!(
            patch
                .text
                .contains("diff --git a/a/nested.txt b/a/nested.txt")
        );
        assert!(
            patch
                .text
                .contains("diff --git a/b/nested.txt b/b/nested.txt")
        );
        assert_eq!(
            crate::split_files(&patch.text)
                .iter()
                .map(|file| file.lines().next().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "diff --git a/a/nested.txt b/a/nested.txt",
                "diff --git a/b/nested.txt b/b/nested.txt",
            ],
            "noprefix={no_prefix}, mnemonicprefix={mnemonic}"
        );
        for path in ["a/nested.txt", "b/nested.txt"] {
            assert!(
                patch
                    .text
                    .contains(&format!("--- a/{path}\n+++ b/{path}\n"))
            );
        }
    }
}

#[test]
fn a_merge_fast_forwards_a_clean_base_and_spares_a_dirty_one() {
    let (root, _, _) = hosted("forward");
    let clone = root.join("clone");
    sh(&root, &["clone", "-q", "--no-local", "hub.git", "clone"]);
    let hub = root.join("hub.git").to_string_lossy().into_owned();
    // The pull request lands on the hub's main.
    let work = root.join("work");
    sh(&work, &["merge", "-q", "--no-edit", "feat"]);
    sh(&work, &["push", "-q", &hub, "main"]);
    let head = sh(&work, &["rev-parse", "HEAD"]);

    std::fs::write(clone.join("a.txt"), "dirty\n").unwrap();
    let said = fast_forward(&clone, "origin", "main").unwrap();
    assert!(said.contains("uncommitted"), "{said}");
    assert_ne!(sh(&clone, &["rev-parse", "main"]), head);

    sh(&clone, &["checkout", "-q", "--", "a.txt"]);
    assert_eq!(
        fast_forward(&clone, "origin", "main").unwrap(),
        "main fast-forwarded."
    );
    assert_eq!(sh(&clone, &["rev-parse", "HEAD"]), head);
}

#[test]
fn a_base_not_checked_out_moves_only_forward() {
    let (root, _, _) = hosted("refmove");
    let clone = root.join("clone");
    sh(&root, &["clone", "-q", "--no-local", "hub.git", "clone"]);
    sh(&clone, &["checkout", "-q", "-b", "elsewhere"]);
    let hub = root.join("hub.git").to_string_lossy().into_owned();
    let work = root.join("work");
    sh(&work, &["merge", "-q", "--no-edit", "feat"]);
    sh(&work, &["push", "-q", &hub, "main"]);
    let merged = sh(&work, &["rev-parse", "HEAD"]);

    assert_eq!(
        fast_forward(&clone, "origin", "main").unwrap(),
        "main fast-forwarded."
    );
    assert_eq!(sh(&clone, &["rev-parse", "main"]), merged);

    // A main with its own commit is not moved.
    sh(&clone, &["checkout", "-q", "main"]);
    std::fs::write(clone.join("mine.txt"), "mine\n").unwrap();
    sh(&clone, &["add", "mine.txt"]);
    sh(&clone, &["commit", "-qm", "mine"]);
    let mine = sh(&clone, &["rev-parse", "main"]);
    sh(&clone, &["checkout", "-q", "elsewhere"]);
    std::fs::write(work.join("more.txt"), "more\n").unwrap();
    sh(&work, &["add", "more.txt"]);
    sh(&work, &["commit", "-qm", "more"]);
    sh(&work, &["push", "-q", &hub, "main"]);
    let said = fast_forward(&clone, "origin", "main").unwrap();
    assert!(said.contains("its own commits"), "{said}");
    assert_eq!(sh(&clone, &["rev-parse", "main"]), mine);
}

#[test]
fn only_object_names_reach_git_as_revisions() {
    let (root, base, _) = hosted("oids");
    let range = Range {
        number: 7,
        base_ref: "main",
        base_oid: &base,
        head_oid: "--output=/tmp/x",
    };
    assert!(fetch_commits(&root.join("work"), "origin", &range).is_err());
}

#[test]
fn the_worktree_on_a_branch_is_found() {
    let (root, _, _) = hosted("worktree");
    let work = root.join("work");
    let linked = root.join("linked");
    sh(
        &work,
        &["worktree", "add", "-q", linked.to_str().unwrap(), "feat"],
    );
    let found = worktree_on(&work, "feat").unwrap();
    assert_eq!(
        found.canonicalize().unwrap(),
        linked.canonicalize().unwrap()
    );
    assert_eq!(worktree_on(&work, "absent"), None);
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

#[test]
fn remote_selection_does_not_cross_hosts() {
    let root = fresh("remote-host");
    sh(&root, &["init", "-q"]);
    sh(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "git@ghe.example.com:acme/widget.git",
        ],
    );
    sh(
        &root,
        &[
            "remote",
            "add",
            "public",
            "https://github.com/acme/widget.git",
        ],
    );
    assert_eq!(remote_for(&root, "acme/widget").as_deref(), Some("public"));
    assert_eq!(
        remote_for_host(&root, "ACME/widget", "GHE.EXAMPLE.COM").as_deref(),
        Some("origin")
    );
    assert_eq!(
        remote_for_host(&root, "acme/widget", "elsewhere.example"),
        None
    );
    assert_eq!(
        host_of_url("ssh://git@ghe.example.com:2222/acme/widget"),
        Some("ghe.example.com")
    );
}

fn remote_state() -> serde_json::Value {
    serde_json::json!({"headRefOid": "head", "baseRefName": "main", "baseRefOid": "base"})
}

fn anchored_read(
    before: &serde_json::Value,
    after: &serde_json::Value,
) -> (GhResult<PatchText>, usize) {
    let range = Range {
        number: 7,
        head_oid: "head",
        base_ref: "main",
        base_oid: "base",
    };
    let mut calls = 0;
    let result = remote_anchored_with("ghe.example.com/acme/widget", &range, |_, args, _| {
        calls += 1;
        assert_eq!(args[4], "ghe.example.com/acme/widget");
        match calls {
            1 => {
                assert_eq!(args[1], "view");
                Ok(serde_json::to_vec(before).unwrap())
            }
            2 => {
                assert_eq!(args[1], "diff");
                Ok(b"expected diff".to_vec())
            }
            3 => {
                assert_eq!(args[1], "view");
                Ok(serde_json::to_vec(after).unwrap())
            }
            _ => panic!("unexpected gh invocation"),
        }
    });
    (result, calls)
}

#[test]
fn an_unchanged_remote_range_returns_the_patch() {
    let (result, calls) = anchored_read(&remote_state(), &remote_state());
    assert_eq!(calls, 3);
    assert_eq!(result.unwrap().text, "expected diff");
}

#[test]
fn a_remote_range_mismatch_refuses_before_reading_the_patch() {
    for key in ["headRefOid", "baseRefName", "baseRefOid"] {
        let mut changed = remote_state();
        changed[key] = serde_json::json!("changed");
        let (result, calls) = anchored_read(&changed, &remote_state());
        assert!(result.is_err(), "{key}");
        assert_eq!(calls, 1, "{key}");
    }
}

#[test]
fn a_push_or_retarget_during_the_patch_read_is_refused() {
    for key in ["headRefOid", "baseRefName", "baseRefOid"] {
        let mut changed = remote_state();
        changed[key] = serde_json::json!("changed");
        let (result, calls) = anchored_read(&remote_state(), &changed);
        assert!(result.is_err(), "{key}");
        assert_eq!(calls, 3, "{key}");
    }
}

#[test]
fn a_missing_remote_anchor_is_not_accepted() {
    let (result, calls) = anchored_read(&serde_json::json!({}), &remote_state());
    assert!(result.is_err());
    assert_eq!(calls, 1);
}

#[test]
fn an_unreadable_remote_anchor_is_refused() {
    let range = Range {
        number: 7,
        head_oid: "head",
        base_ref: "main",
        base_oid: "base",
    };
    assert!(check_remote_range(b"not JSON", &range).is_err());
}

#[test]
fn an_anchored_remote_patch_still_obeys_the_size_cap() {
    let range = Range {
        number: 7,
        head_oid: "head",
        base_ref: "main",
        base_oid: "base",
    };
    let result = remote_anchored_with("acme/widget", &range, |_, args, _| {
        if args[1] == "diff" {
            Ok(vec![b'x'; MAX_PATCH_BYTES + 1])
        } else {
            Ok(serde_json::to_vec(&remote_state()).unwrap())
        }
    });
    assert!(matches!(result, Err(GhError::Failed(message)) if message.contains("too large")));
}
