//! Mouse selection geometry (`phux_web::selection`), under node. The copied
//! text is the engine's: see `phux-vt-web`'s selection test.

use phux_web::selection::{Selection, cell_at};
use wasm_bindgen_test::wasm_bindgen_test;

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
