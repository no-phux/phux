//! Cutting one file's diff to a selection. Hunk headers pass through as
//! they are: `git apply --recount` counts the lines.

use super::{Selection, Side, line_staging_refusal, partial_patch};

#[test]
fn rename_and_copy_metadata_refuses_line_actions_in_both_directions() {
    for kind in ["rename", "copy"] {
        let file = format!(
            "diff --git a/old.txt b/new.txt\n{kind} from old.txt\n{kind} to new.txt\n--- a/old.txt\n+++ b/new.txt\n@@ -1 +1 @@\n-old\n+new\n"
        );
        assert!(line_staging_refusal(&file).is_some());
        for reverse in [false, true] {
            assert!(partial_patch(&file, Selection::lines(Side::New, 1, 1), reverse).is_none());
        }
    }
}

const HEAD: &str =
    "diff --git a/f.txt b/f.txt\nindex 1111111..2222222 100644\n--- a/f.txt\n+++ b/f.txt\n";
const NO_EOL: &str = "\\ No newline at end of file";

/// `a b c d` becoming `a B C d`.
fn replaced() -> String {
    format!("{HEAD}@@ -1,4 +1,4 @@ fn f()\n a\n-b\n-c\n+B\n+C\n d\n")
}

/// Two hunks, so a selection can leave one out.
fn two_hunks() -> String {
    format!("{HEAD}@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n@@ -10,3 +10,4 @@\n j\n-k\n+K\n+K2\n l\n")
}

fn body(text: &str) -> String {
    format!("{HEAD}{text}")
}

fn on(side: Side, start: usize, end: usize) -> Selection {
    Selection::lines(side, start, end)
}

#[test]
fn forward_keeps_selected_lines_and_turns_other_removals_into_context() {
    assert_eq!(
        partial_patch(&replaced(), on(Side::New, 2, 2), false).unwrap(),
        body("@@ -1,4 +1,4 @@ fn f()\n a\n b\n c\n+B\n d\n")
    );
    let removals = body("@@ -1,4 +1,4 @@ fn f()\n a\n-b\n-c\n d\n");
    assert_eq!(
        partial_patch(&replaced(), on(Side::Old, 2, 3), false).unwrap(),
        removals
    );
    assert_eq!(
        partial_patch(&replaced(), on(Side::Old, 3, 2), false).unwrap(),
        removals
    );
}

#[test]
fn reverse_drops_other_removals_and_keeps_other_additions_as_context() {
    assert_eq!(
        partial_patch(&replaced(), on(Side::New, 2, 2), true).unwrap(),
        body("@@ -1,4 +1,4 @@ fn f()\n a\n+B\n C\n d\n")
    );
    assert_eq!(
        partial_patch(&replaced(), on(Side::Old, 2, 3), true).unwrap(),
        body("@@ -1,4 +1,4 @@ fn f()\n a\n-b\n-c\n B\n C\n d\n")
    );
}

#[test]
fn a_range_across_sides_covers_the_patch_body_between_its_ends() {
    // Old line 3 (`-c`) through new line 2 (`+B`), in patch order.
    let selection = on(Side::Old, 3, 2).with_end_side(Side::New);
    assert_eq!(
        partial_patch(&replaced(), selection, false).unwrap(),
        body("@@ -1,4 +1,4 @@ fn f()\n a\n b\n-c\n+B\n d\n")
    );
    let backwards = on(Side::New, 2, 3).with_end_side(Side::Old);
    assert_eq!(
        partial_patch(&replaced(), backwards, false),
        partial_patch(&replaced(), selection, false)
    );
}

#[test]
fn context_alone_is_nothing_to_apply() {
    for reverse in [false, true] {
        assert_eq!(
            partial_patch(&replaced(), on(Side::Old, 1, 1), reverse),
            None
        );
        assert_eq!(
            partial_patch(&replaced(), on(Side::New, 4, 4), reverse),
            None
        );
        let same_line = on(Side::Old, 1, 1).with_end_side(Side::New);
        assert_eq!(partial_patch(&replaced(), same_line, reverse), None);
    }
}

#[test]
fn only_hunks_with_a_selected_change_survive() {
    assert_eq!(
        partial_patch(&two_hunks(), on(Side::New, 11, 12), false).unwrap(),
        body("@@ -10,3 +10,4 @@\n j\n k\n+K\n+K2\n l\n")
    );
    assert_eq!(
        partial_patch(&two_hunks(), on(Side::Old, 11, 11), true).unwrap(),
        body("@@ -10,3 +10,4 @@\n j\n-k\n K\n K2\n l\n")
    );
}

#[test]
fn a_last_line_without_newline_is_re_added_when_lines_follow_it() {
    // `x y` (no newline) becoming `x y z w`; stage only `z`.
    let patch = body(&format!("@@ -1,2 +1,4 @@\n x\n-y\n{NO_EOL}\n+y\n+z\n+w\n"));
    assert_eq!(
        partial_patch(&patch, on(Side::New, 3, 3), false).unwrap(),
        body(&format!("@@ -1,2 +1,4 @@\n x\n-y\n{NO_EOL}\n+y\n+z\n"))
    );
}

#[test]
fn a_no_newline_marker_travels_with_its_line() {
    // `x y z` becoming `x y` (no newline).
    let patch = body(&format!("@@ -1,3 +1,2 @@\n x\n-y\n-z\n+y\n{NO_EOL}\n"));
    assert_eq!(
        partial_patch(&patch, on(Side::Old, 3, 3), false).unwrap(),
        body("@@ -1,3 +1,2 @@\n x\n y\n-z\n")
    );
    assert_eq!(
        partial_patch(&patch, on(Side::Old, 3, 3), true).unwrap(),
        body(&format!("@@ -1,3 +1,2 @@\n x\n-z\n y\n{NO_EOL}\n"))
    );
}

const NEW_FILE: &str = "diff --git a/new.txt b/new.txt\nnew file mode 100644\nindex 0000000..3333333\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,3 @@\n+one\n+two\n+three\n";

#[test]
fn a_new_file_stays_new_unless_its_cut_keeps_lines_on_both_sides() {
    let header = "diff --git a/new.txt b/new.txt\nnew file mode 100644\nindex 0000000..3333333\n--- /dev/null\n+++ b/new.txt\n";
    assert_eq!(
        partial_patch(NEW_FILE, on(Side::New, 2, 2), false).unwrap(),
        format!("{header}@@ -0,0 +1,3 @@\n+two\n")
    );
    // Unstaging one line leaves the file in the index, so it is patched.
    assert_eq!(
        partial_patch(NEW_FILE, on(Side::New, 2, 2), true).unwrap(),
        "diff --git a/new.txt b/new.txt\n--- a/new.txt\n+++ b/new.txt\n@@ -0,0 +1,3 @@\n one\n+two\n three\n"
    );
    assert_eq!(
        partial_patch(NEW_FILE, on(Side::New, 1, 3), true).unwrap(),
        NEW_FILE
    );
}

#[test]
fn part_of_a_deletion_patches_the_file_instead_of_deleting_it() {
    let deleted = "diff --git a/old.txt b/old.txt\ndeleted file mode 100644\nindex 3333333..0000000\n--- a/old.txt\n+++ /dev/null\n@@ -1,3 +0,0 @@\n-one\n-two\n-three\n";
    assert_eq!(
        partial_patch(deleted, on(Side::Old, 2, 2), false).unwrap(),
        "diff --git a/old.txt b/old.txt\n--- a/old.txt\n+++ b/old.txt\n@@ -1,3 +0,0 @@\n one\n-two\n three\n"
    );
    assert_eq!(
        partial_patch(deleted, on(Side::Old, 1, 3), false).unwrap(),
        deleted
    );
}
