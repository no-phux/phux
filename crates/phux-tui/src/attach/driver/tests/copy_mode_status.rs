//! The copy-mode status strip: its selection sentence and width-tiered
//! legend.

use super::*;

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
