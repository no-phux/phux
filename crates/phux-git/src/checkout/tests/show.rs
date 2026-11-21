//! `show`, and the totals beside a truncated list.

use super::*;

#[test]
fn show_answers_message_files_and_patch_against_the_first_parent() {
    let dir = repo("show");
    write(&dir, "a.txt", "1\n");
    write(&dir, "old.txt", "keep\nthese\nlines\n");
    commit(&dir, "base");
    write(&dir, "a.txt", "1\n2\n");
    git(&dir, &["mv", "old.txt", "new.txt"]);
    std::fs::write(dir.join("blob.bin"), [0_u8, 1]).unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "second\n\nthe body"]);
    let sha = git(&dir, &["rev-parse", "HEAD"]);
    let checkout = Checkout::open(&dir).unwrap();

    let detail = checkout.show(&sha.trim()[..12]).unwrap();
    assert_eq!(detail.commit.sha, sha.trim());
    assert_eq!(detail.commit.subject, "second");
    assert_eq!(detail.message, "second\n\nthe body");
    assert!(detail.diff.contains("+2") && detail.diff.contains("b/a.txt"));
    assert!(!detail.truncated);
    let file = |path: &str| detail.files.iter().find(|f| f.path == path).unwrap();
    assert_eq!(
        (file("a.txt").added, file("a.txt").deleted),
        (Some(1), Some(0))
    );
    assert_eq!(file("new.txt").from.as_deref(), Some("old.txt"));
    assert!(file("blob.bin").binary);

    // The root commit shows against nothing.
    let root = git(&dir, &["rev-parse", "HEAD~1"]);
    let first = checkout.show(root.trim()).unwrap();
    assert_eq!(first.files.len(), 2);

    for bad in ["", "abc", "--output=x", "HEAD", &"a".repeat(65)] {
        assert!(
            matches!(checkout.show(bad), Err(CheckoutError::InvalidRequest(_))),
            "{bad:?}"
        );
    }
    assert!(matches!(
        checkout.show("deadbeefdeadbeef"),
        Err(CheckoutError::NotFound(_))
    ));
}

#[test]
fn lists_count_every_entry() {
    let dir = repo("totals");
    write(&dir, "a.txt", "a\n");
    commit(&dir, "base");
    write(&dir, "b.txt", "b\n");
    write(&dir, "c/d.txt", "d\n");
    let checkout = Checkout::open(&dir).unwrap();
    assert_eq!(checkout.status().unwrap().total, Some(2));
    assert_eq!(checkout.branches().unwrap().total, Some(1));
    assert_eq!(checkout.tree("").unwrap().total, Some(3));
}
