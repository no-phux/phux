use super::*;

fn entry(area: Area, path: &str, code: char) -> Entry {
    Entry {
        area,
        path: path.into(),
        from: None,
        code,
    }
}

#[test]
fn porcelain_v2_reads_branch_and_every_kind_of_change() {
    let out = [
        "# branch.oid 4fc60502deadbeefcafe",
        "# branch.head main",
        "# branch.upstream origin/main",
        "# branch.ab +2 -1",
        "1 .M N... 100644 100644 100644 aaa bbb src/app.rs",
        "1 MM N... 100644 100644 100644 aaa bbb docs/with space.md",
        "2 R. N... 100644 100644 100644 aaa bbb R100 src/new.rs",
        "src/old.rs",
        "u UU N... 100644 100644 100644 100644 a b c conflict.txt",
        "? notes/todo.txt",
        "! ignored.log",
        "",
    ]
    .join("\0");
    let status = parse_status(PathBuf::from("/tmp/phux"), &out);
    assert_eq!(
        status.branch,
        Branch {
            name: Some("main".into()),
            commit: Some("4fc60502".into()),
            upstream: Some("origin/main".into()),
            ahead: 2,
            behind: 1,
        }
    );
    let mut renamed = entry(Area::Staged, "src/new.rs", 'R');
    renamed.from = Some("src/old.rs".into());
    assert_eq!(
        status.entries,
        vec![
            entry(Area::Conflicted, "conflict.txt", 'U'),
            entry(Area::Staged, "docs/with space.md", 'M'),
            renamed,
            entry(Area::Unstaged, "docs/with space.md", 'M'),
            entry(Area::Unstaged, "src/app.rs", 'M'),
            entry(Area::Untracked, "notes/todo.txt", '?'),
        ]
    );
    assert_eq!(status.name(), "phux");
}

#[test]
fn a_new_repository_has_no_commit_and_a_detached_head_no_name() {
    let status = parse_status(
        PathBuf::from("/r"),
        "# branch.oid (initial)\0# branch.head (detached)\0",
    );
    assert_eq!(status.branch.commit, None);
    assert_eq!(status.branch.name, None);
    assert!(status.entries.is_empty());
}

#[test]
fn a_row_shows_the_name_before_its_folder() {
    assert_eq!(
        entry(Area::Unstaged, "a/b/c.rs", 'M').split_name(),
        ("c.rs", "a/b")
    );
    assert_eq!(
        entry(Area::Unstaged, "c.rs", 'M').split_name(),
        ("c.rs", "")
    );
}

/// Drives the real `git` binary through a whole stage/commit cycle.
#[test]
fn stage_unstage_and_commit_round_trip_through_git() {
    let dir = std::env::temp_dir().join(format!("phux-git-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let git = |args: &[&str]| run(&dir, args, None).unwrap();
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "Test"]);
    git(&["config", "commit.gpgsign", "false"]);
    git(&["config", "core.hooksPath", "/dev/null"]);
    std::fs::write(dir.join("sub/a.txt"), "one\n").unwrap();

    // Asked from a subfolder, the status is the whole checkout's.
    let status = read_status(&dir.join("sub")).unwrap().unwrap();
    assert_eq!(status.count(Area::Untracked), 1);
    let untracked = status.in_area(Area::Untracked).next().unwrap().clone();
    assert_eq!(untracked.path, "sub/a.txt");
    let preview = read_patch(&status.root, Source::Changes, &[untracked.path], 3).unwrap();
    assert!(
        preview
            .text
            .contains("+++ b/sub/a.txt\n@@ -0,0 +1 @@\n+one\n")
    );

    apply(&status.root, &Op::Stage(vec!["sub/a.txt".into()])).unwrap();
    let status = read_status(&dir).unwrap().unwrap();
    assert_eq!(status.count(Area::Staged), 1);
    // Unstaging works before the first commit.
    apply(&status.root, &Op::Unstage(vec!["sub/a.txt".into()])).unwrap();
    assert_eq!(read_status(&dir).unwrap().unwrap().count(Area::Staged), 0);

    apply(&status.root, &Op::StageAll).unwrap();
    apply(&status.root, &Op::Commit("feat: first\n\nbody".into())).unwrap();
    let status = read_status(&dir).unwrap().unwrap();
    assert!(status.entries.is_empty());
    assert!(status.branch.commit.is_some());
    assert_eq!(status.branch.name.as_deref(), Some("main"));
    assert_eq!(
        git(&["log", "-1", "--format=%B"]).trim(),
        "feat: first\n\nbody"
    );

    std::fs::write(dir.join("sub/a.txt"), "two\n").unwrap();
    let status = read_status(&dir).unwrap().unwrap();
    assert_eq!(status.count(Area::Unstaged), 1);
    let diff = read_patch(&status.root, Source::Changes, &[], 3).unwrap();
    assert!(diff.text.contains("-one\n+two\n"));

    // An empty commit is git's refusal, surfaced as its message.
    let refused = apply(&status.root, &Op::Commit("nothing".into())).unwrap_err();
    assert!(!refused.is_empty());

    assert_eq!(read_status(Path::new("/")).unwrap(), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_rename_cut_off_before_its_source_is_dropped() {
    let out = [
        "# branch.head main",
        "1 .M N... 100644 100644 100644 aaa bbb kept.rs",
        "2 R. N... 100644 100644 100644 aaa bbb R100 new.rs",
        "",
    ]
    .join("\0");
    let status = parse_status(PathBuf::from("/tmp/phux"), &out);
    let paths: Vec<_> = status.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["kept.rs"]);
}
