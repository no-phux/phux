//! Line staging and conflict resolution against the real `git` binary.

use super::*;

/// A fresh repository named for its test, so tests can run in parallel.
fn repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("phux-git-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "test@example.com"],
        &["config", "user.name", "Test"],
        &["config", "commit.gpgsign", "false"],
        &["config", "core.hooksPath", "/dev/null"],
    ] {
        git(&dir, args);
    }
    dir
}

fn git(dir: &Path, args: &[&str]) -> String {
    run(dir, args, None).unwrap()
}

fn write(dir: &Path, path: &str, text: &str) {
    std::fs::write(dir.join(path), text).unwrap();
}

fn commit_all(dir: &Path, message: &str) {
    apply(dir, &Op::StageAll).unwrap();
    apply(dir, &Op::Commit(message.into())).unwrap();
}

/// Cut `source`'s one changed file to `selection` and apply it to the index.
fn apply_selection(dir: &Path, source: Source, untracked: &[String], selection: &Selection) {
    let reverse = source == Source::Staged;
    let patch = read_patch(dir, source, untracked, 3).unwrap();
    let [file] = split_files(&patch.text)[..] else {
        panic!("one file in {}", patch.text);
    };
    let cut = partial_patch(file, *selection, reverse).unwrap();
    apply(
        dir,
        &Op::Apply {
            patch: cut,
            reverse,
        },
    )
    .unwrap();
}

#[test]
fn one_of_two_changed_lines_is_staged_and_unstaged_again() {
    let dir = repo("line");
    write(&dir, "f.txt", "a\nb\nc\nd\n");
    commit_all(&dir, "base");
    write(&dir, "f.txt", "a\nx\nb\nc\nd\ny\n");

    let x = Selection::lines(Side::New, 2, 2);
    apply_selection(&dir, Source::Changes, &[], &x);
    let staged = git(&dir, &["diff", "--cached", "--no-color"]);
    assert!(
        staged.contains("\n+x\n") && !staged.contains("+y"),
        "{staged}"
    );
    assert_eq!(git(&dir, &["show", ":f.txt"]), "a\nx\nb\nc\nd\n");
    // The rest is still a change in the working tree.
    let left = read_patch(&dir, Source::Changes, &[], 3).unwrap();
    assert!(
        left.text.contains("\n+y\n") && !left.text.contains("+x"),
        "{}",
        left.text
    );

    apply_selection(&dir, Source::Staged, &[], &x);
    assert_eq!(git(&dir, &["diff", "--cached"]), "");
    assert_eq!(
        std::fs::read_to_string(dir.join("f.txt")).unwrap(),
        "a\nx\nb\nc\nd\ny\n"
    );

    // A replaced line, staged as its removal and addition together.
    write(&dir, "f.txt", "a\nB\nc\nD\n");
    let pair = Selection::lines(Side::Old, 2, 2).with_end_side(Side::New);
    apply_selection(&dir, Source::Changes, &[], &pair);
    assert_eq!(git(&dir, &["show", ":f.txt"]), "a\nB\nc\nd\n");
    apply_selection(&dir, Source::Staged, &[], &pair);
    assert_eq!(git(&dir, &["diff", "--cached"]), "");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn part_of_an_untracked_file_is_staged_as_a_new_file() {
    let dir = repo("untracked");
    write(&dir, "base.txt", "base\n");
    commit_all(&dir, "base");
    write(&dir, "new.txt", "one\ntwo\nthree\n");
    let untracked = vec!["new.txt".to_owned()];

    let first_two = Selection::lines(Side::New, 1, 2);
    apply_selection(&dir, Source::Changes, &untracked, &first_two);
    assert_eq!(git(&dir, &["show", ":new.txt"]), "one\ntwo\n");
    let status = read_status(&dir).unwrap().unwrap();
    assert_eq!(status.count(Area::Staged), 1);
    assert_eq!(status.count(Area::Unstaged), 1);

    // Unstaging one line of the newly added file keeps the file.
    let two = Selection::lines(Side::New, 2, 2);
    apply_selection(&dir, Source::Staged, &[], &two);
    assert_eq!(git(&dir, &["show", ":new.txt"]), "one\n");
    // Unstaging all that is left removes it from the index again.
    let one = Selection::lines(Side::New, 1, 1);
    apply_selection(&dir, Source::Staged, &[], &one);
    let status = read_status(&dir).unwrap().unwrap();
    assert_eq!(status.count(Area::Staged), 0);
    assert_eq!(status.count(Area::Untracked), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_line_after_a_last_line_without_newline_can_be_staged() {
    let dir = repo("eol");
    write(&dir, "n.txt", "x\ny");
    commit_all(&dir, "base");
    write(&dir, "n.txt", "x\ny\nz\nw\n");
    let z = Selection::lines(Side::New, 3, 3);
    apply_selection(&dir, Source::Changes, &[], &z);
    assert_eq!(git(&dir, &["show", ":n.txt"]), "x\ny\nz\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn patches_cover_both_sources_before_the_first_commit() {
    let dir = repo("read");
    write(&dir, "a.txt", "alpha\n");
    apply(&dir, &Op::Stage(vec!["a.txt".into()])).unwrap();
    write(&dir, "a.txt", "alpha\nbeta\n");
    write(&dir, "b.txt", "bravo\n");
    std::fs::write(dir.join("bin.dat"), b"\0\x01\x02binary").unwrap();
    std::fs::create_dir_all(dir.join("folder")).unwrap();

    // An unborn HEAD diffs against the empty tree.
    let staged = read_patch(&dir, Source::Staged, &[], 3).unwrap();
    assert!(!staged.reduced);
    assert!(staged.text.contains("new file mode"), "{}", staged.text);
    assert!(
        staged.text.contains("+++ b/a.txt\n@@ -0,0 +1 @@\n+alpha\n"),
        "{}",
        staged.text
    );

    let untracked: Vec<String> = ["b.txt", "bin.dat", "folder", "gone.txt"]
        .map(String::from)
        .to_vec();
    let changes = read_patch(&dir, Source::Changes, &untracked, 3).unwrap();
    assert!(!changes.reduced);
    let text = &changes.text;
    assert!(
        text.contains("+++ b/a.txt\n@@ -1 +1,2 @@\n alpha\n+beta\n"),
        "{text}"
    );
    assert!(
        text.contains("--- /dev/null\n+++ b/b.txt\n@@ -0,0 +1 @@\n+bravo\n"),
        "{text}"
    );
    assert!(
        !text.contains("bin.dat") && !text.contains("folder"),
        "{text}"
    );

    // Untracked paths past the cap are left out, and say so.
    let many: Vec<String> = (0..=MAX_UNTRACKED)
        .map(|n| format!("missing-{n}"))
        .collect();
    assert!(read_patch(&dir, Source::Changes, &many, 3).unwrap().reduced);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_conflict_is_resolved_with_the_text_given() {
    let dir = repo("conflict");
    write(&dir, "f.txt", "base\n");
    commit_all(&dir, "base");
    git(&dir, &["checkout", "-q", "-b", "other"]);
    write(&dir, "f.txt", "theirs\n");
    commit_all(&dir, "theirs");
    git(&dir, &["checkout", "-q", "main"]);
    write(&dir, "f.txt", "ours\n");
    write(&dir, "g.txt", "plain\n");
    commit_all(&dir, "ours");
    assert!(run(&dir, &["merge", "-q", "other"], None).is_err());
    write(&dir, "g.txt", "plain\nedit\n");

    let status = read_status(&dir).unwrap().unwrap();
    assert_eq!(status.count(Area::Conflicted), 1);
    // The unmerged path's combined diff stays out of the patch.
    let changes = read_patch(&dir, Source::Changes, &[], 3).unwrap().text;
    assert!(
        !changes.contains("diff --cc") && !changes.contains("f.txt"),
        "{changes}"
    );
    assert!(changes.contains("+edit"), "{changes}");
    let staged = read_patch(&dir, Source::Staged, &[], 3).unwrap().text;
    assert!(
        !staged.contains("Unmerged") && !staged.contains("f.txt"),
        "{staged}"
    );

    let text = read_file(&dir, "f.txt").unwrap();
    assert!(
        text.contains("<<<<<<<") && text.contains(">>>>>>>"),
        "{text}"
    );
    let resolve = Op::Resolve {
        path: "f.txt".into(),
        expected: text,
        text: "ours and theirs\n".into(),
    };
    apply(&dir, &resolve).unwrap();
    let status = read_status(&dir).unwrap().unwrap();
    assert_eq!(status.count(Area::Conflicted), 0);
    assert_eq!(git(&dir, &["show", ":f.txt"]), "ours and theirs\n");

    assert!(read_file(&dir, "../f.txt").is_err());
    assert!(read_file(&dir, "/etc/hosts").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stale_resolution_keeps_newer_worktree_text() {
    let dir = repo("stale-conflict");
    write(&dir, "f.txt", "newer edit\n");
    let error = apply(
        &dir,
        &Op::Resolve {
            path: "f.txt".into(),
            expected: "old conflict\n".into(),
            text: "resolution\n".into(),
        },
    )
    .unwrap_err();
    assert!(error.contains("changed since it was reviewed"), "{error}");
    assert_eq!(
        std::fs::read_to_string(dir.join("f.txt")).unwrap(),
        "newer edit\n"
    );
    assert_eq!(git(&dir, &["diff", "--cached"]), "");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_actions_treat_wildcards_as_literal_paths() {
    let dir = repo("literal-paths");
    write(&dir, "f*.txt", "base\n");
    write(&dir, "first.txt", "base\n");
    commit_all(&dir, "base");
    write(&dir, "f*.txt", "literal edit\n");
    write(&dir, "first.txt", "other edit\n");
    apply(&dir, &Op::Stage(vec!["f*.txt".into()])).unwrap();
    assert_eq!(git(&dir, &["show", ":first.txt"]), "base\n");
    assert_eq!(git(&dir, &["show", ":f*.txt"]), "literal edit\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn non_utf8_patch_bytes_are_refused_without_mutating_the_index() {
    let dir = repo("non-utf8-patch");
    write(&dir, "tracked.txt", "base\nsafe\n");
    commit_all(&dir, "base");
    std::fs::write(dir.join("new.txt"), b"\xff\nsafe\n").unwrap();
    let before = git(&dir, &["ls-files", "--stage"]);
    let error = read_patch(&dir, Source::Changes, &["new.txt".into()], 3).unwrap_err();
    assert!(error.contains("non-UTF-8"), "{error}");
    assert_eq!(git(&dir, &["ls-files", "--stage"]), before);

    // Whole-file staging is byte preserving; only text patch construction
    // refuses the unsupported encoding. Status remains available too.
    apply(&dir, &Op::Stage(vec!["new.txt".into()])).unwrap();
    let bytes = command(&dir, &["show", ":new.txt"]).output().unwrap();
    assert!(bytes.status.success());
    assert_eq!(bytes.stdout, b"\xff\nsafe\n");
    let staged = git(&dir, &["ls-files", "--stage"]);
    assert!(read_patch(&dir, Source::Staged, &[], 3).is_err());
    assert_eq!(git(&dir, &["ls-files", "--stage"]), staged);

    std::fs::write(dir.join("tracked.txt"), b"\xff\nsafe\n").unwrap();
    assert!(read_patch(&dir, Source::Changes, &[], 3).is_err());
    assert!(read_status(&dir).unwrap().is_some());
    let bytes = command(&dir, &["show", ":tracked.txt"]).output().unwrap();
    assert!(bytes.status.success());
    assert_eq!(bytes.stdout, b"base\nsafe\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn line_actions_preserve_the_current_index_mode_in_both_directions() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = repo("content-only-mode");
    git(&dir, &["config", "core.filemode", "true"]);
    write(&dir, "f.txt", "one\ntwo\nthree\n");
    std::fs::set_permissions(dir.join("f.txt"), std::fs::Permissions::from_mode(0o644)).unwrap();
    commit_all(&dir, "base");
    write(&dir, "f.txt", "ONE\ntwo\nTHREE\n");
    std::fs::set_permissions(dir.join("f.txt"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let selection = Selection::lines(Side::Old, 1, 1).with_end_side(Side::New);
    let patch = read_patch(&dir, Source::Changes, &[], 3).unwrap();
    assert!(patch.text.contains("old mode 100644\nnew mode 100755\n"));
    apply_selection(&dir, Source::Changes, &[], &selection);
    assert!(git(&dir, &["ls-files", "--stage"]).starts_with("100644 "));
    assert_eq!(git(&dir, &["show", ":f.txt"]), "ONE\ntwo\nthree\n");

    apply(&dir, &Op::StageAll).unwrap();
    let patch = read_patch(&dir, Source::Staged, &[], 3).unwrap();
    assert!(patch.text.contains("old mode 100644\nnew mode 100755\n"));
    apply_selection(&dir, Source::Staged, &[], &selection);
    assert!(git(&dir, &["ls-files", "--stage"]).starts_with("100755 "));
    assert_eq!(git(&dir, &["ls-files"]), "f.txt\n");
    assert_eq!(git(&dir, &["show", ":f.txt"]), "one\ntwo\nTHREE\n");
    assert_eq!(
        std::fs::read_to_string(dir.join("f.txt")).unwrap(),
        "ONE\ntwo\nTHREE\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn partial_unstaging_refuses_a_rename_without_relocating_index_paths() {
    let dir = repo("content-only-rename");
    write(
        &dir,
        "old.txt",
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n",
    );
    commit_all(&dir, "base");
    git(&dir, &["mv", "old.txt", "new.txt"]);
    write(
        &dir,
        "new.txt",
        "ONE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nTEN\n",
    );
    apply(&dir, &Op::StageAll).unwrap();
    let before = git(&dir, &["ls-files", "--stage"]);
    let patch = read_patch(&dir, Source::Staged, &[], 3).unwrap();
    assert!(
        patch
            .text
            .contains("rename from old.txt\nrename to new.txt\n"),
        "{}",
        patch.text
    );
    assert!(line_staging_refusal(&patch.text).is_some());
    let selection = Selection::lines(Side::Old, 1, 1).with_end_side(Side::New);
    assert!(partial_patch(&patch.text, selection, true).is_none());
    assert_eq!(git(&dir, &["ls-files", "--stage"]), before);
    assert_eq!(git(&dir, &["ls-files"]), "new.txt\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn conflict_reads_and_writes_refuse_symlinks_in_any_component() {
    let dir = repo("symlink-conflict");
    let outside = repo("symlink-conflict-target");
    write(&outside, "f.txt", "keep me\n");
    std::os::unix::fs::symlink(outside.join("f.txt"), dir.join("file-link")).unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("directory-link")).unwrap();
    for path in ["file-link", "directory-link/f.txt"] {
        assert!(read_file(&dir, path).is_err());
        assert!(
            apply(
                &dir,
                &Op::Resolve {
                    path: path.into(),
                    expected: "keep me\n".into(),
                    text: "overwrite\n".into(),
                }
            )
            .is_err()
        );
    }
    assert_eq!(
        std::fs::read_to_string(outside.join("f.txt")).unwrap(),
        "keep me\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn a_patch_git_rejects_surfaces_its_message() {
    let dir = repo("reject");
    write(&dir, "f.txt", "a\n");
    commit_all(&dir, "base");
    let bogus = "diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,1 +1,1 @@\n-zzz\n+a\n";
    let error = apply(
        &dir,
        &Op::Apply {
            patch: bogus.into(),
            reverse: false,
        },
    )
    .unwrap_err();
    assert!(error.contains("patch does not apply"), "{error}");
    let _ = std::fs::remove_dir_all(&dir);
}
