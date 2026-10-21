//! Checkout reads and writes against the real `git` binary.

use super::*;

/// A fresh repository named for its test, so tests run in parallel.
fn repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("phux-checkout-{name}-{}", std::process::id()));
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
    // The canonical top level, as `rev-parse --show-toplevel` reports it.
    dir.canonicalize().unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = run_git(dir, args, 1 << 20, None, Mode::Read).unwrap();
    assert!(out.status.success(), "git {args:?}: {}", out.stderr);
    String::from_utf8(out.stdout).unwrap()
}

fn write(dir: &Path, path: &str, text: &str) {
    let file = dir.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
}

fn commit(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

fn change<'a>(snapshot: &'a Snapshot, area: Area, path: &str) -> &'a Change {
    snapshot
        .changes
        .iter()
        .find(|change| change.area == area && change.path == path)
        .unwrap_or_else(|| panic!("{area:?} {path} in {snapshot:#?}"))
}

#[test]
fn open_finds_the_top_level_from_a_subdirectory_and_refuses_elsewhere() {
    let dir = repo("open");
    write(&dir, "src/lib.rs", "x\n");
    let checkout = Checkout::open(&dir.join("src")).unwrap();
    assert_eq!(checkout.root(), dir);

    let outside = std::env::temp_dir().join(format!("phux-checkout-none-{}", std::process::id()));
    std::fs::create_dir_all(&outside).unwrap();
    // A temp directory may itself sit in a checkout on some hosts; only
    // assert the refusal when it does not.
    if run_git(&outside, &["rev-parse"], 64, None, Mode::Read)
        .is_ok_and(|out| !out.status.success())
    {
        assert_eq!(Checkout::open(&outside), Err(CheckoutError::NotARepository));
    }
    assert!(matches!(
        Checkout::open(&dir.join("missing")),
        Err(CheckoutError::NotFound(_))
    ));
    assert_eq!(
        Checkout::open(&dir.join(".git")),
        Err(CheckoutError::NotARepository)
    );
}

#[test]
fn status_counts_lines_per_area_including_untracked_and_binary() {
    let dir = repo("status");
    write(&dir, "a.txt", "1\n2\n3\n");
    write(&dir, "gone.txt", "bye\n");
    commit(&dir, "base");
    write(&dir, "a.txt", "1\nTWO\n3\n4\n");
    git(&dir, &["add", "a.txt"]);
    write(&dir, "a.txt", "1\nTWO\n3\n4\n5\n");
    std::fs::remove_file(dir.join("gone.txt")).unwrap();
    write(&dir, "new dir/note.md", "a\nb");
    std::fs::write(dir.join("blob.bin"), [0_u8, 1, 2]).unwrap();

    let snapshot = Checkout::open(&dir).unwrap().status().unwrap();
    assert_eq!(snapshot.branch.as_deref(), Some("main"));
    assert_eq!(snapshot.head.as_ref().map(String::len), Some(40));
    assert!(!snapshot.truncated);

    let staged = change(&snapshot, Area::Staged, "a.txt");
    assert_eq!((staged.added, staged.deleted), (Some(2), Some(1)));
    let unstaged = change(&snapshot, Area::Unstaged, "a.txt");
    assert_eq!((unstaged.added, unstaged.deleted), (Some(1), Some(0)));
    let gone = change(&snapshot, Area::Unstaged, "gone.txt");
    assert_eq!((gone.code, gone.deleted), ('D', Some(1)));
    let note = change(&snapshot, Area::Untracked, "new dir/note.md");
    assert_eq!((note.added, note.deleted), (Some(2), Some(0)));
    let blob = change(&snapshot, Area::Untracked, "blob.bin");
    assert!(blob.binary && blob.added.is_none());
}

#[test]
fn status_before_the_first_commit_has_no_head() {
    let dir = repo("unborn");
    write(&dir, "a.txt", "x\n");
    git(&dir, &["add", "a.txt"]);
    let snapshot = Checkout::open(&dir).unwrap().status().unwrap();
    assert_eq!(snapshot.head, None);
    let added = change(&snapshot, Area::Staged, "a.txt");
    assert_eq!((added.code, added.added), ('A', Some(1)));
}

#[test]
fn renames_count_under_their_destination() {
    let counts = Counts::parse(b"3\t1\tsrc/a.rs\0-\t-\timg.png\0" as &[u8], false);
    assert_eq!(counts.get("src/a.rs"), Some(&(Some(3), Some(1), false)));
    assert_eq!(counts.get("img.png"), Some(&(None, None, true)));
    let renamed = Counts::parse(b"0\t0\t\0old.rs\0new.rs\0", false);
    assert_eq!(renamed.get("new.rs"), Some(&(Some(0), Some(0), false)));
    assert_eq!(renamed.get("old.rs"), None);
}

#[test]
fn diff_shows_one_file_per_area_and_untracked_whole() {
    let dir = repo("diff");
    write(&dir, "a.txt", "1\n");
    write(&dir, "b.txt", "b\n");
    commit(&dir, "base");
    write(&dir, "a.txt", "1\nstaged\n");
    git(&dir, &["add", "a.txt"]);
    write(&dir, "a.txt", "1\nstaged\nworking\n");
    write(&dir, "b.txt", "B\n");
    write(&dir, "fresh.txt", "hello\n");
    let checkout = Checkout::open(&dir).unwrap();

    let staged = checkout.diff("a.txt", Area::Staged).unwrap();
    assert!(staged.text.contains("+staged") && !staged.text.contains("+working"));
    assert!(!staged.text.contains("b.txt"));
    let unstaged = checkout.diff("a.txt", Area::Unstaged).unwrap();
    assert!(unstaged.text.contains("+working") && !unstaged.text.contains("+staged"));
    let fresh = checkout.diff("fresh.txt", Area::Untracked).unwrap();
    assert!(fresh.text.contains("+hello"), "{}", fresh.text);
    assert!(fresh.text.contains("b/fresh.txt"));
    assert!(!fresh.truncated && !fresh.binary);

    std::fs::write(dir.join("blob.bin"), [0_u8, 1, 2]).unwrap();
    assert!(checkout.diff("blob.bin", Area::Untracked).unwrap().binary);
    assert!(matches!(
        checkout.diff("nope.txt", Area::Untracked),
        Err(CheckoutError::NotFound(_))
    ));
}

#[test]
fn paths_outside_the_checkout_are_refused() {
    for path in [
        "",
        "/etc/passwd",
        "../x",
        "a/../../x",
        "./a",
        ".git/config",
        "a/.GIT/x",
    ] {
        assert!(
            matches!(relative(path), Err(CheckoutError::InvalidPath(_))),
            "{path:?}"
        );
    }
    assert_eq!(relative("src/main.rs"), Ok("src/main.rs"));

    let dir = repo("escape");
    let outside = dir.with_extension("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret"), "s\n").unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();
    std::os::unix::fs::symlink(outside.join("secret"), dir.join("file-link")).unwrap();
    let checkout = Checkout::open(&dir).unwrap();
    assert!(matches!(
        checkout.read("link/secret", 0),
        Err(CheckoutError::InvalidPath(_))
    ));
    assert!(matches!(
        checkout.read("file-link", 0),
        Err(CheckoutError::InvalidPath(_))
    ));
    assert!(matches!(
        checkout.diff("link/secret", Area::Untracked),
        Err(CheckoutError::InvalidPath(_))
    ));
    assert!(matches!(
        checkout.tree("link"),
        Err(CheckoutError::InvalidPath(_))
    ));
    assert!(matches!(
        checkout.stage(&["../x".to_owned()]),
        Err(CheckoutError::InvalidPath(_))
    ));
    assert!(matches!(
        checkout.stage(&[]),
        Err(CheckoutError::InvalidRequest(_))
    ));
}

#[test]
fn log_pages_newest_first_with_refs_and_path_filter() {
    let dir = repo("log");
    assert_eq!(
        Checkout::open(&dir).unwrap().log(0, 10, None).unwrap(),
        LogPage::default()
    );
    for n in 0..5 {
        write(
            &dir,
            if n % 2 == 0 { "even.txt" } else { "odd.txt" },
            &n.to_string(),
        );
        commit(&dir, &format!("commit {n}"));
    }
    git(&dir, &["tag", "v1"]);
    let checkout = Checkout::open(&dir).unwrap();

    let first = checkout.log(0, 2, None).unwrap();
    assert!(first.has_more);
    let subjects: Vec<_> = first.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects, ["commit 4", "commit 3"]);
    let head = &first.commits[0];
    assert_eq!(head.sha.len(), 40);
    assert!(head.sha.starts_with(&head.short_sha));
    assert_eq!(head.author_name, "Test");
    assert_eq!(head.author_email, "test@example.com");
    assert!(head.time > 0);
    assert!(
        head.refs.iter().any(|r| r == "HEAD -> main"),
        "{:?}",
        head.refs
    );
    assert!(head.refs.iter().any(|r| r == "tag: v1"));

    let last = checkout.log(4, 2, None).unwrap();
    assert!(!last.has_more);
    assert_eq!(last.commits.len(), 1);

    let odd = checkout.log(0, 100, Some("odd.txt")).unwrap();
    let subjects: Vec<_> = odd.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects, ["commit 3", "commit 1"]);
}

#[test]
fn branches_list_locals_first_with_tracking() {
    let dir = repo("branches");
    write(&dir, "a.txt", "a\n");
    commit(&dir, "base");
    git(&dir, &["branch", "feature"]);
    // A fake remote-tracking branch one commit behind main.
    let base = git(&dir, &["rev-parse", "HEAD"]);
    git(
        &dir,
        &["update-ref", "refs/remotes/origin/main", base.trim()],
    );
    git(
        &dir,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(&dir, &["config", "remote.origin.url", "/dev/null"]);
    git(
        &dir,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    git(&dir, &["config", "branch.main.remote", "origin"]);
    git(&dir, &["config", "branch.main.merge", "refs/heads/main"]);
    write(&dir, "a.txt", "b\n");
    commit(&dir, "ahead");

    let list = Checkout::open(&dir).unwrap().branches().unwrap();
    assert_eq!(list.current.as_deref(), Some("main"));
    let names: Vec<_> = list
        .branches
        .iter()
        .map(|b| (b.name.as_str(), b.remote))
        .collect();
    // Locals first, then remotes; within each, newest first, and commits in
    // the same second tie, so only the grouping is fixed here.
    assert_eq!(names.len(), 3);
    assert!(names[..2].contains(&("main", false)));
    assert!(names[..2].contains(&("feature", false)));
    assert_eq!(names[2], ("origin/main", true));
    let branch = |name: &str| list.branches.iter().find(|b| b.name == name).unwrap();
    let main = branch("main");
    assert!(main.current);
    assert_eq!(main.upstream.as_deref(), Some("origin/main"));
    assert_eq!((main.ahead, main.behind), (Some(1), Some(0)));
    assert_eq!(main.subject, "ahead");
    let feature = branch("feature");
    assert!(!feature.current && feature.upstream.is_none() && feature.ahead.is_none());
}

#[test]
fn tree_lists_files_and_directories_with_ignored_marks() {
    let dir = repo("tree");
    write(&dir, ".gitignore", "target/\n*.log\n");
    write(&dir, "src/main.rs", "fn main() {}\n");
    write(&dir, "target/out", "x");
    write(&dir, "debug.log", "x");
    write(&dir, "README.md", "hi\n");
    let checkout = Checkout::open(&dir).unwrap();

    let top = checkout.tree("").unwrap();
    let names: Vec<_> = top
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e.kind, e.ignored))
        .collect();
    assert_eq!(
        names,
        [
            ("src", EntryKind::Directory, false),
            ("target", EntryKind::Directory, true),
            (".gitignore", EntryKind::File, false),
            ("README.md", EntryKind::File, false),
            ("debug.log", EntryKind::File, true),
        ]
    );
    assert_eq!(top.entries[3].size, Some(3));

    write(&dir, "src/scratch.log", "x");
    let src = checkout.tree("src/").unwrap();
    assert_eq!(src.entries.len(), 2);
    assert!(
        src.entries
            .iter()
            .any(|e| e.name == "scratch.log" && e.ignored)
    );
    std::fs::remove_file(dir.join("src/scratch.log")).unwrap();
    let src = checkout.tree("src").unwrap();
    assert_eq!(src.entries.len(), 1);
    assert_eq!(src.entries[0].name, "main.rs");
    assert!(matches!(
        checkout.tree("nope"),
        Err(CheckoutError::NotFound(_))
    ));
}

#[test]
fn read_returns_text_bounded_and_flags_binary() {
    let dir = repo("read");
    write(&dir, "a.txt", "héllo\nworld\n");
    std::fs::write(dir.join("blob.bin"), [1_u8, 0, 2]).unwrap();
    let checkout = Checkout::open(&dir).unwrap();

    let whole = checkout.read("a.txt", 0).unwrap();
    assert_eq!(whole.text, "héllo\nworld\n");
    assert!(!whole.truncated && !whole.binary);
    assert_eq!(whole.size, 13);

    // Two bytes cut `é` in half: keep the text before it.
    let cut = checkout.read("a.txt", 2).unwrap();
    assert_eq!(cut.text, "h");
    assert!(cut.truncated);

    let blob = checkout.read("blob.bin", 0).unwrap();
    assert!(blob.binary && blob.text.is_empty());
    assert!(matches!(
        checkout.read("missing.txt", 0),
        Err(CheckoutError::NotFound(_))
    ));
}

#[test]
fn stage_and_unstage_move_paths_through_the_index() {
    let dir = repo("stage");
    write(&dir, "a.txt", "a\n");
    write(&dir, "b.txt", "b\n");
    let checkout = Checkout::open(&dir).unwrap();

    // Before the first commit, unstage takes paths out of the index.
    checkout
        .stage(&["a.txt".to_owned(), "b.txt".to_owned()])
        .unwrap();
    checkout.unstage(&["b.txt".to_owned()]).unwrap();
    let snapshot = checkout.status().unwrap();
    change(&snapshot, Area::Staged, "a.txt");
    change(&snapshot, Area::Untracked, "b.txt");

    commit(&dir, "base");
    write(&dir, "a.txt", "A\n");
    checkout.stage(&["a.txt".to_owned()]).unwrap();
    assert_eq!(
        change(&checkout.status().unwrap(), Area::Staged, "a.txt").code,
        'M'
    );
    checkout.unstage(&["a.txt".to_owned()]).unwrap();
    let snapshot = checkout.status().unwrap();
    change(&snapshot, Area::Unstaged, "a.txt");
    assert!(snapshot.changes.iter().all(|c| c.area != Area::Staged));

    assert!(matches!(
        checkout.stage(&["no-such-file".to_owned()]),
        Err(CheckoutError::Git(_))
    ));
}

#[test]
fn text_cut_keeps_whole_lines() {
    assert_eq!(text_at_line(b"a\nb\nc", true), ("a\nb\n".to_owned(), true));
    assert_eq!(
        text_at_line(b"a\nb\nc", false),
        ("a\nb\nc".to_owned(), false)
    );
}

#[test]
fn commit_and_branch_records_parse() {
    let commit =
        parse_commit("abc\x1fab\x1fAnn\x1fa@x\x1f17\x1f\x1fsubject with \x1f inside").unwrap();
    assert_eq!(commit.subject, "subject with \x1f inside");
    assert!(commit.refs.is_empty());
    assert!(
        parse_branch(
            "refs/remotes/origin/HEAD\x1forigin\x1f\x1f\x1fabc\x1f1\x1fs",
            None
        )
        .is_none()
    );
    let gone = parse_branch(
        "refs/heads/x\x1fx\x1forigin/x\x1fgone\x1fabc\x1f1\x1fs",
        None,
    )
    .unwrap();
    assert_eq!((gone.ahead, gone.behind), (None, None));
    assert_eq!(tracking("ahead 3, behind 2"), (Some(3), Some(2)));
    assert_eq!(tracking(""), (Some(0), Some(0)));
}

#[test]
fn an_overrunning_write_is_stopped_without_leaving_index_lock() {
    let dir = repo("lock");
    write(&dir, "slow.txt", "x\n");
    git(&dir, &["config", "filter.slow.clean", "sleep 30; cat"]);
    write(&dir, ".gitattributes", "slow.txt filter=slow\n");
    let started = std::time::Instant::now();
    let out = run_git(
        &dir,
        &["add", "--", "slow.txt"],
        1024,
        None,
        Mode::Write(Duration::from_millis(500)),
    );
    assert!(matches!(out, Err(CheckoutError::Timeout)), "{out:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(
        !dir.join(".git/index.lock").exists(),
        "SIGTERM let git remove its lock"
    );
}

#[test]
fn a_spoofed_dotgit_name_is_refused_and_hidden() {
    let dir = repo("dotgit");
    write(&dir, "sub/.GIT/config", "x");
    let checkout = Checkout::open(&dir).unwrap();
    assert!(matches!(
        checkout.read("sub/.GIT/config", 0),
        Err(CheckoutError::InvalidPath(_))
    ));
    let names: Vec<_> = checkout
        .tree("sub")
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert!(names.is_empty(), "{names:?}");
}
