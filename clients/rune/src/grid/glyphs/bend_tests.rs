//! Rounded corners painted through Rune's renderer: each ends square on its
//! piece's open edges, where the straight lines it meets join it, rounds at
//! its corner and paints nothing past its piece, in every corner, in thin
//! and ordinary cells, at every scale.

use super::*;
use phux_client_runtime::draw::boxes::Cell;

type Scene = (Rect, Bend, f32);

fn view(scene: &Scene, _: &mut rune::Cx<Scene>) -> El {
    let (rect, line, scale) = *scene;
    el().child(bent(rect, line, scale, Color::WHITE))
}

/// The piece `ch` draws in `cell`, painted alone: its alpha per device
/// pixel, `width` to a row.
fn paint(gpu: &rune::render::Gpu, ch: char, cell: Cell, scale: f32) -> (Piece, Vec<u8>, usize) {
    let pieces = boxes::pieces(ch, cell).expect("a drawn corner");
    let [piece] = pieces[..] else {
        panic!("{ch} is one piece: {pieces:?}")
    };
    let size = ((cell.right + 2) as u32, (cell.bottom + 2) as u32);
    let viewport = (size.0 as f32 / scale, size.1 as f32 / scale);
    let scene = (piece.rect, piece.bend.expect("a bend"), scale);
    let env = rune::Env::new(false, scale);
    let mut rt = crate::test_support::runtime(scene, view, viewport, env);
    rt.frame(0.0);
    let mut renderer = rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
    let rgba = crate::ui::offscreen(gpu, &mut renderer, &mut rt, size);
    let alpha = rgba.iter().skip(3).step_by(4).copied().collect();
    (piece, alpha, size.0 as usize)
}

#[test]
fn rounded_corners_end_square_on_their_open_edges() {
    let gpu = rune::render::Gpu::headless();
    for scale in [1.0_f32, 1.25, 1.5, 2.0, 3.0] {
        // A thin cell, an ordinary one and a large one, in points.
        for (w, h) in [(4.0_f32, 9.0_f32), (7.8, 17.0), (12.0, 26.0)] {
            let device = |points: f32| (points * scale).round() as i32;
            let cell = Cell {
                left: 0,
                top: 0,
                right: device(w),
                bottom: device(h),
            };
            for (corner, ch) in ['╭', '╮', '╯', '╰'].into_iter().enumerate() {
                let (piece, alpha, width) = paint(&gpu, ch, cell, scale);
                let at = |x: i32, y: i32| alpha[y as usize * width + x as usize];
                let (left, top, right, bottom) = piece.rect;
                let line = piece.bend.expect("a bend");
                let t = line.thickness;
                let tag = format!("{ch} in {w}x{h} at {scale}x");
                // The horizontal arm's row and the vertical arm's column.
                let (row, col) = match corner {
                    0 => (top, left),
                    1 => (top, right - t),
                    2 => (bottom - t, right - t),
                    _ => (bottom - t, left),
                };
                let open_x = if matches!(corner, 0 | 3) {
                    right - 1
                } else {
                    left
                };
                let open_y = if corner < 2 { bottom - 1 } else { top };
                // An arm no wider than two lines is all arc, so it meets
                // the edge at its tangent, antialiased.
                if right - left > 2 * t {
                    for y in 0..alpha.len() as i32 / width as i32 {
                        let want = if (row..row + t).contains(&y) { 255 } else { 0 };
                        assert_eq!(at(open_x, y), want, "{tag}: horizontal end, row {y}");
                    }
                }
                if bottom - top > 2 * t {
                    for x in 0..width as i32 {
                        let want = if (col..col + t).contains(&x) { 255 } else { 0 };
                        assert_eq!(at(x, open_y), want, "{tag}: vertical end, column {x}");
                    }
                }
                for (i, &a) in alpha.iter().enumerate() {
                    let (x, y) = ((i % width) as i32, (i / width) as i32);
                    let within = (left..right).contains(&x) && (top..bottom).contains(&y);
                    assert!(within || a == 0, "{tag}: paints ({x}, {y}) past its piece");
                }
                let (cx, cy) = match corner {
                    0 => (left, top),
                    1 => (right - 1, top),
                    2 => (right - 1, bottom - 1),
                    _ => (left, bottom - 1),
                };
                if line.radius > 2 * t {
                    assert!(at(cx, cy) < 128, "{tag}: square at its corner");
                }
            }
        }
    }
}
