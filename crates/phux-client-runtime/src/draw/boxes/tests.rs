use super::*;

fn grid() -> Cell {
    Cell {
        left: 0,
        top: 0,
        right: 20,
        bottom: 44,
    }
}

fn covered(rects: &[Rect], x: i32, y: i32) -> bool {
    rects
        .iter()
        .any(|(l, t, r, b)| x >= *l && x < *r && y >= *t && y < *b)
}

#[test]
fn the_table_covers_every_line_character() {
    for code in 0x2500..=0x257f {
        let ch = char::from_u32(code).unwrap();
        assert!(shape(ch).is_some(), "U+{code:04X}");
    }
}

#[test]
fn arms_reach_the_cell_edges_and_meet() {
    // └: the stem leaves the top edge and the arm reaches the right edge.
    let rects = arms_rects(&grid(), 2, [1, 1, 0, 0]);
    assert!(covered(&rects, 10, 0), "stem touches the top");
    assert!(covered(&rects, 19, 22), "arm touches the right");
    assert!(covered(&rects, 9, 22), "the corner is filled");
    assert!(!covered(&rects, 10, 43), "nothing below the corner");
}

#[test]
fn doubles_join_as_two_lines() {
    // ╔: outer and inner L, with the gap between them open at the corner.
    let rects = arms_rects(&grid(), 2, [0, 3, 3, 0]);
    let (x0, _) = band(0, 20, 2, 3);
    let (y0, _) = band(0, 44, 2, 3);
    assert!(covered(&rects, 19, y0), "outer line reaches the right");
    assert!(!covered(&rects, 19, y0 + 2), "gap between the lines");
    assert!(covered(&rects, 19, y0 + 4), "inner line reaches the right");
    assert!(covered(&rects, x0, y0), "outer corner");
    assert!(!covered(&rects, x0 + 2, y0 + 2), "the gaps meet inside");
    assert!(covered(&rects, x0 + 4, y0 + 4), "inner corner");
    // ╞: the single vertical stays whole across the double arm.
    let rects = arms_rects(&grid(), 2, [1, 3, 1, 0]);
    let (v0, _) = band(0, 20, 2, 1);
    for y in 0..44 {
        assert!(covered(&rects, v0, y), "╞ stem row {y}");
    }
}

#[test]
fn sextants_skip_the_half_blocks() {
    assert_eq!(shape('\u{1fb00}'), Some(Shape::Sextant(1)));
    // U+1FB13 would be 20; 21 is ▌, so U+1FB14 is 22.
    assert_eq!(shape('\u{1fb14}'), Some(Shape::Sextant(22)));
    assert_eq!(shape('\u{1fb3b}'), Some(Shape::Sextant(62)));
}

#[test]
fn powerline_arrows_fill_to_both_edges() {
    let pieces = raster(&grid(), 2, Outline::Arrow { right: true });
    let first_row: f32 = pieces
        .iter()
        .filter(|(r, _)| r.1 == 0)
        .map(|(r, a)| (r.2 - r.0) as f32 * a)
        .sum();
    let middle: f32 = pieces
        .iter()
        .filter(|(r, _)| r.1 == 22)
        .map(|(r, a)| (r.2 - r.0) as f32 * a)
        .sum();
    assert!(first_row > 0., "the tip row still has ink");
    assert!(
        middle > 19.,
        "the middle row reaches the right edge: {middle}"
    );
    assert!(
        pieces.iter().any(|(r, _)| r.3 == 44),
        "reaches the bottom edge"
    );
}

#[test]
fn dashes_are_even() {
    let rects = dash_rects(&grid(), 2, false, 1, 4);
    let widths: Vec<_> = rects.iter().map(|r| r.2 - r.0).collect();
    assert_eq!(widths, [4, 4, 4, 4]);
}

#[test]
fn pieces_carry_bends_shades_and_drop_nothing_drawn() {
    let bent = pieces('\u{256d}', grid()).expect("a corner");
    let [piece] = bent.as_slice() else {
        panic!("one piece")
    };
    assert_eq!(piece.bend.map(|bend| bend.corner), Some(0));
    let shade = pieces('\u{2592}', grid()).expect("a shade");
    assert!((shade[0].coverage - 0.5).abs() < f32::EPSILON);
    assert_eq!(pieces('a', grid()), None);
    assert!(draws('\u{e0b0}') && draws('\u{2800}') && !draws('\u{e0a0}'));
    for code in 0x2500..=0x257f {
        let ch = char::from_u32(code).unwrap();
        let drawn = pieces(ch, grid()).expect("a line character");
        assert!(!drawn.is_empty(), "U+{code:04X}");
        for piece in drawn {
            let (l, t, r, b) = piece.rect;
            assert!(l >= 0 && t >= 0 && r <= 20 && b <= 44 && r > l && b > t);
        }
    }
}
