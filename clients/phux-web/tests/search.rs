//! Find in the terminal (`phux_web::search`), under node.

use phux_web::Mark;
use phux_web::search::{MATCH_LIMIT, Match, Search, find_matches, reveal_row};
use wasm_bindgen_test::wasm_bindgen_test;

/// The engine's width table for the characters these tests use.
fn width(ch: char) -> u8 {
    match ch {
        '\u{4e2d}' | '\u{6587}' => 2,
        '\u{301}' => 0,
        _ => 1,
    }
}

fn rows(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| (*line).to_owned()).collect()
}

const fn hit(row: u64, start: u16, end: u16) -> Match {
    Match { row, start, end }
}

#[wasm_bindgen_test]
fn matches_are_row_ordered_ascii_case_insensitive_cells() {
    let screen = rows(&["make build", "", "Build ok: BUILD", "rebuilding"]);
    let (found, truncated) = find_matches(&screen, "build", width);
    assert!(!truncated);
    assert_eq!(
        found,
        [hit(0, 5, 10), hit(2, 0, 5), hit(2, 10, 15), hit(3, 2, 7)]
    );
    assert_eq!(find_matches(&screen, "", width).0, []);
    assert_eq!(find_matches(&screen, "absent", width).0, []);
}

#[wasm_bindgen_test]
fn wide_and_combining_characters_occupy_their_cells() {
    let screen = rows(&["\u{4e2d}\u{6587} ok", "e\u{301}t\u{e9} ok"]);
    let (found, _) = find_matches(&screen, "ok", width);
    assert_eq!(found, [hit(0, 5, 7), hit(1, 4, 6)], "columns count cells");
    let (found, _) = find_matches(&screen, "\u{6587}", width);
    assert_eq!(found, [hit(0, 2, 4)], "a wide match covers both cells");
}

#[wasm_bindgen_test]
fn a_huge_result_is_capped_and_marked_truncated() {
    let screen: Vec<String> = (0..MATCH_LIMIT + 5).map(|_| "x".to_owned()).collect();
    let (found, truncated) = find_matches(&screen, "x", width);
    assert_eq!(found.len(), MATCH_LIMIT);
    assert!(truncated);
}

#[wasm_bindgen_test]
fn a_new_query_starts_at_the_newest_match_and_steps_wrap() {
    let mut search = Search::default();
    assert_eq!(search.label(), "");
    search.set_results("x", vec![hit(1, 0, 1), hit(5, 2, 3), hit(9, 0, 1)], false);
    assert_eq!(search.current(), Some(hit(9, 0, 1)), "newest first");
    assert_eq!(search.label(), "3 of 3");
    assert_eq!(search.step(true), Some(hit(5, 2, 3)), "older");
    assert_eq!(search.step(true), Some(hit(1, 0, 1)));
    assert_eq!(search.step(true), Some(hit(9, 0, 1)), "wraps to the newest");
    assert_eq!(
        search.step(false),
        Some(hit(1, 0, 1)),
        "newer wraps to the oldest"
    );

    // New output: the same query keeps its current match where it survives.
    search.set_results(
        "x",
        vec![hit(1, 0, 1), hit(5, 2, 3), hit(9, 0, 1), hit(12, 0, 1)],
        false,
    );
    assert_eq!(search.current(), Some(hit(1, 0, 1)));
    assert_eq!(search.label(), "1 of 4");
    search.set_results("x", vec![hit(12, 0, 1)], true);
    assert_eq!(
        search.current(),
        Some(hit(12, 0, 1)),
        "clamped when it is gone"
    );
    assert_eq!(search.label(), "1 of 1+");

    search.set_results("y", Vec::new(), false);
    assert_eq!(search.current(), None);
    assert_eq!(search.label(), "No matches");
    assert_eq!(search.step(true), None);
}

#[wasm_bindgen_test]
fn marks_cover_only_matches_in_the_viewport() {
    let mut search = Search::default();
    search.set_results(
        "ab",
        vec![hit(2, 1, 3), hit(10, 0, 2), hit(11, 4, 6)],
        false,
    );
    // Viewport shows screen rows 10..14 of a 6-column grid.
    assert_eq!(
        search.marks(10, 4, 6),
        [
            Mark {
                cells: 0..2,
                current: false
            },
            Mark {
                cells: 10..12,
                current: true
            },
        ]
    );
    assert_eq!(search.marks(0, 2, 6), [], "nothing on rows 0..2");
}

#[wasm_bindgen_test]
fn revealing_a_row_centers_it_only_when_it_is_off_screen() {
    // 100 screen rows, viewport of 10 at the live screen (offset 90).
    assert_eq!(reveal_row(95, 90, 10, 100), None, "already visible");
    assert_eq!(reveal_row(20, 90, 10, 100), Some(15), "centered");
    assert_eq!(reveal_row(2, 90, 10, 100), Some(0), "clamped at the top");
    assert_eq!(
        reveal_row(99, 0, 10, 100),
        Some(90),
        "clamped at the live screen"
    );
}
