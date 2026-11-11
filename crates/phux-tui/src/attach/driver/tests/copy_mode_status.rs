//! The copy-mode status strip: its selection sentence and width-tiered
//! legend.

use super::*;

/// The copy-mode strip counts a block selection as rows x band columns
/// (12 here, with `start_col > end_col` not underflowing), distinct from the
/// linear count (3).
#[test]
fn copy_mode_status_block_cell_count_differs_from_linear() {
    let theme = crate::render::Theme::default();
    let status_of = |sel: SelectionRect| {
        let mut out: Vec<u8> = Vec::new();
        let content = crate::layout::Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 24,
        };
        paint_copy_mode_status(&mut out, sel, None, content, (80, 24), &theme).expect("status");
        String::from_utf8_lossy(&out).into_owned()
    };
    let rect = |start_row, start_col, end_row, end_col, rectangle| SelectionRect {
        start_row,
        start_col,
        end_row,
        end_col,
        rectangle,
    };
    assert!(status_of(rect(0, 5, 2, 2, true)).contains("· 12 cells "));
    assert!(status_of(rect(0, 5, 2, 2, false)).contains("· 3 cells "));
    assert!(status_of(rect(1, 2, 3, 6, true)).contains("· 15 cells "));
}

/// Paint the copy-mode strip over `pane_cols` columns and return its visible
/// text (escape sequences removed).
fn strip_text(sel: SelectionRect, search: Option<&str>, pane_cols: u16) -> String {
    let theme = crate::render::Theme::default();
    let content = crate::layout::Rect {
        x: 0,
        y: 0,
        w: pane_cols,
        h: 24,
    };
    let mut out: Vec<u8> = Vec::new();
    paint_copy_mode_status(&mut out, sel, search, content, (pane_cols, 24), &theme)
        .expect("status");
    let raw = String::from_utf8_lossy(&out).into_owned();
    let mut text = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for e in chars.by_ref() {
                if e.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            text.push(c);
        }
    }
    text
}

fn one_cell() -> SelectionRect {
    SelectionRect {
        start_row: 0,
        start_col: 3,
        end_row: 0,
        end_col: 3,
        rectangle: false,
    }
}

/// The strip reads as a sentence, pluralises the cell count, and appends the
/// search text as given.
#[test]
fn copy_mode_status_reads_as_a_sentence() {
    assert!(strip_text(one_cell(), None, 80).contains(" Copy mode · linear · 1 cell "));
    let block = SelectionRect {
        start_row: 0,
        start_col: 5,
        end_row: 2,
        end_col: 2,
        rectangle: true,
    };
    assert!(strip_text(block, None, 80).contains(" Copy mode · block · 12 cells "));
    assert!(
        strip_text(one_cell(), Some("/3_"), 80).contains(" Copy mode · linear · 1 cell · /3_ ")
    );
}

/// The legend degrades by tier and is dropped only when nothing fits; the
/// strip is always exactly the pane width.
#[test]
fn copy_mode_legend_degrades_with_width() {
    let full = strip_text(one_cell(), None, 90);
    assert!(full.contains("y copy · v select · V line · / find · tab block · esc"));
    let mid = strip_text(one_cell(), None, 61);
    assert!(mid.contains("⏎ copy") && mid.contains("esc") && !mid.contains("v select"));
    let narrow = strip_text(one_cell(), None, 30);
    assert!(!narrow.contains("⏎ copy"));
    assert_eq!(crate::render::display_width(&narrow), 30);
    assert_eq!(crate::render::display_width(&mid), 61);
}

/// `legend_for` picks the longest tier that fits and drops the legend when
/// even the shortest does not.
#[test]
fn legend_for_picks_the_longest_tier_that_fits() {
    assert_eq!(legend_for(200), Some(COPY_MODE_LEGENDS[0]));
    assert_eq!(legend_for(0), None);
    assert_eq!(legend_for(13), None);
    assert_eq!(legend_for(14), Some("⏎ copy · esc"));
}
