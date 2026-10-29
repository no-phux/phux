use gpui_kit::component::diff::{DiffLineRange, DiffSide};

use crate::git::github::LineSide;

use super::lines::{patch_row, thread_lines};
use super::*;

#[test]
fn github_times_read_as_unix_milliseconds() {
    assert_eq!(iso_ms("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(iso_ms("2026-10-23T16:25:00Z"), Some(1_792_772_700_000));
    assert_eq!(iso_ms("2024-02-29T12:00:00.123Z"), Some(1_709_208_000_000));
    assert_eq!(iso_ms("soon"), None);
}

/// Two change blocks: line 2 is replaced, then line 9 is replaced.
const SECTION: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,10 +1,10 @@\n one\n-two\n+TWO\n three\n four\n five\n six\n seven\n eight\n-nine\n+NINE\n ten\n";

#[test]
fn patch_rows_follow_each_side() {
    assert_eq!(patch_row(SECTION, LineSide::Left, 2), Some(5));
    assert_eq!(patch_row(SECTION, LineSide::Right, 2), Some(6));
    assert_eq!(patch_row(SECTION, LineSide::Right, 3), Some(7));
    assert_eq!(patch_row(SECTION, LineSide::Left, 9), Some(13));
    assert_eq!(patch_row(SECTION, LineSide::Right, 9), Some(14));
    assert_eq!(patch_row(SECTION, LineSide::Right, 40), None);
}

#[test]
fn ranges_become_github_lines_start_first() {
    let up = DiffLineRange::new("a.rs", DiffSide::Modified, 9, 4);
    assert_eq!(
        thread_lines(&up, None),
        ((LineSide::Right, 4), (LineSide::Right, 9))
    );
    // Within one block the removed line comes first.
    let across =
        DiffLineRange::new("a.rs", DiffSide::Modified, 2, 2).with_end_side(DiffSide::Original);
    assert_eq!(
        thread_lines(&across, Some(SECTION)),
        ((LineSide::Left, 2), (LineSide::Right, 2))
    );
    // Across blocks, the earlier block's added line comes before the later removal.
    let blocks =
        DiffLineRange::new("a.rs", DiffSide::Original, 9, 2).with_end_side(DiffSide::Modified);
    assert_eq!(
        thread_lines(&blocks, Some(SECTION)),
        ((LineSide::Right, 2), (LineSide::Left, 9))
    );
}

#[test]
fn pane_commands_close_reviews_and_window_ones_do_not() {
    assert!(hides_reviews("new"));
    assert!(hides_reviews("split-right"));
    assert!(!hides_reviews("reviews"));
    assert!(!hides_reviews("palette"));
}

fn reviewed() -> Opened {
    let pull = super::fixture::pull();
    let mut opened = Opened::new(
        Key {
            repo: pull.repo.clone(),
            number: pull.number,
        },
        1,
    );
    opened.patch.data = Some(Patch {
        from: PatchFrom::GitHub,
        loaded: super::super::changes::review::Loaded::parse(
            crate::git::PatchText {
                text: SECTION.into(),
                reduced: false,
            },
            Vec::new(),
        ),
        head: pull.head_oid.clone(),
        base_ref: pull.base_ref.clone(),
        base_oid: pull.base_oid.clone(),
    });
    opened.pull.data = Some(pull);
    opened
}

#[test]
fn line_notes_pin_the_diff_until_they_are_submitted_or_removed() {
    let mut opened = reviewed();
    opened.pending.push(Pending {
        id: 1,
        range: DiffLineRange::new("a.rs", DiffSide::Modified, 2, 2),
        body: "Keep this on the original head".into(),
    });
    opened.pull.data.as_mut().unwrap().head_oid = "new-head".into();
    assert!(opened.revision_changed());
    assert!(!opened.needs_patch());
    opened.pending.clear();
    assert!(opened.needs_patch());
}

#[test]
fn retargeting_or_advancing_the_base_invalidates_the_reviewed_patch() {
    let mut opened = reviewed();
    assert!(!opened.needs_patch());
    opened.pull.data.as_mut().unwrap().base_ref = "release".into();
    assert!(opened.revision_changed());
    assert!(opened.base_changed());
    assert!(opened.needs_patch());
    opened.pull.data.as_mut().unwrap().base_ref = "main".into();
    opened.pull.data.as_mut().unwrap().base_oid = "new-base".into();
    assert!(opened.base_changed());
    assert!(opened.needs_patch());
}

#[test]
fn a_retarget_between_polls_is_refused_before_the_write() {
    let cached = super::fixture::pull();
    let mut remote = cached.clone();
    remote.base_ref = "release".into();
    assert!(
        super::verify_review_base_with(&cached, |repo, number| {
            assert_eq!(repo, cached.repo);
            assert_eq!(number, cached.number);
            Ok(remote)
        })
        .is_err()
    );
    assert!(super::verify_review_base_with(&cached, |_, _| Ok(cached.clone())).is_ok());
}
