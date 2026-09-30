//! Mouse selection geometry and copied text (`phux_web::selection`), under node.

use phux_vt_web::{Grid, GridCell, Rgb};
use phux_web::selection::{Selection, cell_at};
use wasm_bindgen_test::wasm_bindgen_test;

fn grid(rows: &[&str], cols: u16) -> Grid {
    let mut cells = Vec::new();
    for row in rows {
        let mut chars: Vec<char> = row.chars().collect();
        chars.resize(usize::from(cols), ' ');
        cells.extend(chars.into_iter().map(|ch| GridCell {
            ch,
            fg: None,
            bg: None,
        }));
    }
    let black = Rgb { r: 0, g: 0, b: 0 };
    Grid {
        cols,
        rows: rows.len() as u16,
        default_fg: black,
        default_bg: black,
        cells,
        cursor_col: 0,
        cursor_row: 0,
        cursor_visible: true,
    }
}

#[wasm_bindgen_test]
fn a_drag_selects_the_row_major_run_in_either_direction() {
    let forward = Selection {
        anchor: (2, 0),
        head: (1, 1),
    };
    assert_eq!(forward.cells(10), 2..12);
    let backward = Selection {
        anchor: (1, 1),
        head: (2, 0),
    };
    assert_eq!(backward.cells(10), 2..12, "direction does not matter");
    assert!(Selection::at((3, 3)).is_click());
    assert!(!forward.is_click());
}

#[wasm_bindgen_test]
fn copied_text_is_one_trimmed_line_per_row() {
    let screen = grid(&["$ echo hi", "hi", "$ ls -la  "], 12);
    let selection = Selection {
        anchor: (2, 0),
        head: (4, 2),
    };
    assert_eq!(selection.text(&screen), "echo hi\nhi\n$ ls");
    let whole = Selection {
        anchor: (0, 2),
        head: (11, 2),
    };
    assert_eq!(whole.text(&screen), "$ ls -la", "trailing blanks trimmed");
}

#[wasm_bindgen_test]
fn pointer_positions_map_to_clamped_cells() {
    assert_eq!(cell_at(0.0, 0.0, 8.0, 16.0, 80, 24), (0, 0));
    assert_eq!(cell_at(17.0, 33.0, 8.0, 16.0, 80, 24), (2, 2));
    assert_eq!(
        cell_at(-5.0, -5.0, 8.0, 16.0, 80, 24),
        (0, 0),
        "left of the canvas"
    );
    assert_eq!(
        cell_at(10_000.0, 10_000.0, 8.0, 16.0, 80, 24),
        (79, 23),
        "past the far edge"
    );
}
