//! The panel's read and line-staging path against a real checkout: what
//! `read_checkout` hands the Kit diff view, and that a range picked in that
//! view stages exactly its lines.

use std::path::{Path, PathBuf};
use std::process::Command;

use gpui_kit::component::diff::{DiffLineRange, DiffSide};

use super::*;
use crate::git::Op;

fn repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("phux-panel-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "test@example.com"],
        &["config", "user.name", "Test"],
        &["config", "commit.gpgsign", "false"],
        &["config", "core.hooksPath", "/dev/null"],
    ] {
        run(&dir, args);
    }
    dir
}

fn run(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn paths(loaded: &Loaded) -> Vec<String> {
    loaded
        .files
        .iter()
        .map(|file| file.path().to_string())
        .collect()
}

#[test]
fn a_checkout_reads_into_both_comparisons_and_a_picked_line_stages_alone() {
    let dir = repo("pipeline");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let base: String = (1..=40).map(|n| format!("line {n}\n")).collect();
    std::fs::write(dir.join("src/lib.rs"), &base).unwrap();
    std::fs::write(dir.join("staged.txt"), "one\n").unwrap();
    run(&dir, &["add", "-A"]);
    run(&dir, &["commit", "-q", "-m", "base"]);

    // Two separate edits far apart, a staged edit, and a new file.
    let edited = base
        .replace("line 5\n", "line five\n")
        .replace("line 35\n", "line thirty-five\n");
    std::fs::write(dir.join("src/lib.rs"), &edited).unwrap();
    std::fs::write(dir.join("staged.txt"), "one\ntwo\n").unwrap();
    run(&dir, &["add", "staged.txt"]);
    std::fs::write(dir.join("notes.md"), "# Notes\n").unwrap();

    let snapshot = read_checkout(&dir).unwrap().expect("a checkout");
    assert_eq!(paths(&snapshot.changes), ["src/lib.rs", "notes.md"]);
    assert_eq!(paths(&snapshot.staged), ["staged.txt"]);
    assert_eq!(snapshot.changes.stats("src/lib.rs"), Some((2, 2)));
    assert_eq!(snapshot.changes.stats("notes.md"), Some((1, 0)));
    // Wide context: the whole file is in the patch for the view to unfold.
    assert!(snapshot.changes.text.contains(" line 20\n"));

    // The unified view's selection of line 5's removal and replacement.
    let range = DiffLineRange::new("src/lib.rs", DiffSide::Original, 5, 5)
        .with_end_side(DiffSide::Modified);
    let patch =
        crate::git::partial_patch(&snapshot.changes.text, &review::selection(&range), false)
            .expect("changed lines");
    crate::git::apply(
        &snapshot.status.root,
        &Op::Apply {
            patch,
            reverse: false,
        },
    )
    .unwrap();
    let cached = run(&dir, &["diff", "--cached", "--", "src/lib.rs"]);
    assert!(cached.contains("-line 5\n+line five\n"), "{cached}");
    assert!(!cached.contains("thirty-five"), "{cached}");

    // The staged comparison now carries it; unstaging it out of there reverses.
    let snapshot = read_checkout(&dir).unwrap().unwrap();
    assert_eq!(paths(&snapshot.staged), ["src/lib.rs", "staged.txt"]);
    let back = crate::git::partial_patch(&snapshot.staged.text, &review::selection(&range), true)
        .expect("staged lines");
    crate::git::apply(
        &snapshot.status.root,
        &Op::Apply {
            patch: back,
            reverse: true,
        },
    )
    .unwrap();
    assert!(run(&dir, &["diff", "--cached", "--", "src/lib.rs"]).is_empty());
}
