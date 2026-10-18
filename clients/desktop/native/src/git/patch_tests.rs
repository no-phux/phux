use super::{Selection, Side, partial_patch};

const HEAD: &str =
    "diff --git a/f.txt b/f.txt\nindex 1111111..2222222 100644\n--- a/f.txt\n+++ b/f.txt\n";

/// `a b c d` becoming `a B C d`: one run of two removals, two additions.
fn replaced() -> String {
    format!("{HEAD}@@ -1,4 +1,4 @@ fn f()\n a\n-b\n-c\n+B\n+C\n d\n")
}

/// Two hunks; the second adds a line, so later starts drift.
fn two_hunks() -> String {
    format!("{HEAD}@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n@@ -10,3 +10,4 @@\n j\n-k\n+K\n+K2\n l\n")
}

fn cut(patch: &str, selection: &Selection, reverse: bool) -> Option<String> {
    partial_patch(patch, selection, reverse)
}

fn body(text: &str) -> String {
    format!("{HEAD}{text}")
}

fn on(side: Side, start: usize, end: usize) -> Selection {
    Selection::lines("f.txt", side, start, end)
}

#[test]
fn forward_keeps_a_selected_addition_and_turns_other_removals_into_context() {
    assert_eq!(
        cut(&replaced(), &on(Side::New, 2, 2), false).unwrap(),
        body("@@ -1,4 +1,5 @@ fn f()\n a\n b\n c\n+B\n d\n")
    );
}

#[test]
fn forward_stages_a_range_of_removals_in_either_order() {
    let expected = body("@@ -1,4 +1,2 @@ fn f()\n a\n-b\n-c\n d\n");
    assert_eq!(
        cut(&replaced(), &on(Side::Old, 2, 3), false).unwrap(),
        expected
    );
    assert_eq!(
        cut(&replaced(), &on(Side::Old, 3, 2), false).unwrap(),
        expected
    );
}

#[test]
fn a_range_across_sides_covers_the_patch_body_between_its_ends() {
    // Old line 3 (`-c`) through new line 2 (`+B`), in patch order.
    let selection = on(Side::Old, 3, 2).with_end_side(Side::New);
    assert_eq!(
        cut(&replaced(), &selection, false).unwrap(),
        body("@@ -1,4 +1,4 @@ fn f()\n a\n b\n-c\n+B\n d\n")
    );
    assert_eq!(
        cut(&replaced(), &selection, true).unwrap(),
        body("@@ -1,4 +1,4 @@ fn f()\n a\n-c\n+B\n C\n d\n")
    );
    // Ends given backwards select the same span.
    let backwards = on(Side::New, 2, 3).with_end_side(Side::Old);
    assert_eq!(
        cut(&replaced(), &backwards, false),
        cut(&replaced(), &selection, false)
    );
}

#[test]
fn reverse_drops_other_removals_and_keeps_other_additions_as_context() {
    assert_eq!(
        cut(&replaced(), &on(Side::New, 2, 2), true).unwrap(),
        body("@@ -1,3 +1,4 @@ fn f()\n a\n+B\n C\n d\n")
    );
    assert_eq!(
        cut(&replaced(), &on(Side::Old, 2, 3), true).unwrap(),
        body("@@ -1,6 +1,4 @@ fn f()\n a\n-b\n-c\n B\n C\n d\n")
    );
}

#[test]
fn a_selection_of_context_alone_is_nothing_to_apply() {
    for reverse in [false, true] {
        assert_eq!(cut(&replaced(), &on(Side::Old, 1, 1), reverse), None);
        assert_eq!(cut(&replaced(), &on(Side::New, 4, 4), reverse), None);
        let same_line = on(Side::Old, 1, 1).with_end_side(Side::New);
        assert_eq!(cut(&replaced(), &same_line, reverse), None);
    }
    // A path the patch does not name.
    let elsewhere = Selection::lines("other.txt", Side::New, 2, 2);
    assert_eq!(cut(&replaced(), &elsewhere, false), None);
}

#[test]
fn only_the_selected_hunk_survives() {
    assert_eq!(
        cut(&two_hunks(), &on(Side::New, 11, 12), false).unwrap(),
        body("@@ -10,3 +10,5 @@\n j\n k\n+K\n+K2\n l\n")
    );
    assert_eq!(
        cut(&two_hunks(), &on(Side::Old, 2, 2), false).unwrap(),
        body("@@ -1,3 +1,2 @@\n a\n-b\n c\n")
    );
    assert_eq!(
        cut(&two_hunks(), &on(Side::Old, 11, 11), true).unwrap(),
        body("@@ -10,5 +10,4 @@\n j\n-k\n K\n K2\n l\n")
    );
}

#[test]
fn later_hunks_start_where_earlier_cuts_leave_them() {
    assert_eq!(
        cut(&two_hunks(), &on(Side::New, 2, 12), false).unwrap(),
        body("@@ -1,3 +1,4 @@\n a\n b\n+B\n c\n@@ -10,3 +11,5 @@\n j\n k\n+K\n+K2\n l\n")
    );
    assert_eq!(
        cut(&two_hunks(), &on(Side::Old, 2, 11), true).unwrap(),
        body("@@ -1,4 +1,3 @@\n a\n-b\n B\n c\n@@ -11,5 +10,4 @@\n j\n-k\n K\n K2\n l\n")
    );
}

const NO_EOL: &str = "\\ No newline at end of file";

#[test]
fn a_last_line_without_newline_is_re_added_when_lines_follow_it() {
    // `x y` (no newline) becoming `x y z w`; stage only `z`.
    let patch = body(&format!("@@ -1,2 +1,4 @@\n x\n-y\n{NO_EOL}\n+y\n+z\n+w\n"));
    assert_eq!(
        cut(&patch, &on(Side::New, 3, 3), false).unwrap(),
        body(&format!("@@ -1,2 +1,3 @@\n x\n-y\n{NO_EOL}\n+y\n+z\n"))
    );
}

#[test]
fn a_no_newline_marker_follows_its_line_or_goes_with_it() {
    // `x y z` becoming `x y` (no newline).
    let patch = body(&format!("@@ -1,3 +1,2 @@\n x\n-y\n-z\n+y\n{NO_EOL}\n"));
    // Forward: the unselected addition and its marker are dropped.
    assert_eq!(
        cut(&patch, &on(Side::Old, 3, 3), false).unwrap(),
        body("@@ -1,3 +1,2 @@\n x\n y\n-z\n")
    );
    // Reverse: it stays as the last context line, marker and all.
    assert_eq!(
        cut(&patch, &on(Side::Old, 3, 3), true).unwrap(),
        body(&format!("@@ -1,3 +1,2 @@\n x\n-z\n y\n{NO_EOL}\n"))
    );
    // Staging the addition alone keeps its marker.
    assert_eq!(
        cut(&patch, &on(Side::New, 2, 2), false).unwrap(),
        body(&format!("@@ -1,3 +1,4 @@\n x\n y\n z\n+y\n{NO_EOL}\n"))
    );
}

const NEW_FILE: &str = "diff --git a/new.txt b/new.txt\nnew file mode 100644\nindex 0000000..3333333\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,3 @@\n+one\n+two\n+three\n";

#[test]
fn part_of_a_new_file_is_created_with_only_its_selected_lines() {
    let selection = Selection::lines("new.txt", Side::New, 2, 2);
    let header = "diff --git a/new.txt b/new.txt\nnew file mode 100644\nindex 0000000..3333333\n--- /dev/null\n+++ b/new.txt\n";
    assert_eq!(
        partial_patch(NEW_FILE, &selection, false).unwrap(),
        format!("{header}@@ -0,0 +1,1 @@\n+two\n")
    );
}

#[test]
fn unstaging_part_of_a_new_file_patches_it_instead_of_deleting_it() {
    let one = Selection::lines("new.txt", Side::New, 2, 2);
    assert_eq!(
        partial_patch(NEW_FILE, &one, true).unwrap(),
        "diff --git a/new.txt b/new.txt\n--- a/new.txt\n+++ b/new.txt\n@@ -1,2 +1,3 @@\n one\n+two\n three\n"
    );
    // Every line: the file goes, so it stays a new-file patch.
    let all = Selection::lines("new.txt", Side::New, 1, 3);
    assert_eq!(partial_patch(NEW_FILE, &all, true).unwrap(), NEW_FILE);
}

#[test]
fn the_selected_file_is_found_among_several_and_by_quoted_name() {
    let quoted = "diff --git \"a/caf\\303\\251.txt\" \"b/caf\\303\\251.txt\"\n--- \"a/caf\\303\\251.txt\"\n+++ \"b/caf\\303\\251.txt\"\n@@ -1 +1 @@\n-old\n+new\n";
    let patch = format!("{}{quoted}", replaced());
    let selection = Selection::lines("caf\u{e9}.txt", Side::New, 1, 1);
    let out = partial_patch(&patch, &selection, false).unwrap();
    assert!(out.starts_with("diff --git \"a/caf"), "{out}");
    assert!(out.ends_with("@@ -1,1 +1,2 @@\n old\n+new\n"), "{out}");
}

#[test]
fn staging_part_of_a_deletion_patches_the_file_instead_of_deleting_it() {
    let deleted = "diff --git a/old.txt b/old.txt\ndeleted file mode 100644\nindex 3333333..0000000\n--- a/old.txt\n+++ /dev/null\n@@ -1,3 +0,0 @@\n-one\n-two\n-three\n";
    let two = Selection::lines("old.txt", Side::Old, 2, 2);
    assert_eq!(
        partial_patch(deleted, &two, false).unwrap(),
        "diff --git a/old.txt b/old.txt\n--- a/old.txt\n+++ b/old.txt\n@@ -1,3 +1,2 @@\n one\n-two\n three\n"
    );
    let all = Selection::lines("old.txt", Side::Old, 1, 3);
    assert_eq!(partial_patch(deleted, &all, false).unwrap(), deleted);
}
