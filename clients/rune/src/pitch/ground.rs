//! The pitch's still background, the prototype's `render`: the stands and
//! their crowd, the ground, the markings, each squad's cones and dugout, and
//! the floodlights. Painted once per layout, day and night apart, with
//! tiny-skia as the prototype paints an offscreen canvas, so a frame shows
//! it as one image instead of re-tessellating thousands of seats.

use std::f32::consts::{PI, TAU};

use tiny_skia::{
    BlendMode, Color, FillRule, GradientStop, Paint, Path, PathBuilder, Pixmap, Point,
    RadialGradient, Rect, SpreadMode, Stroke, StrokeDash, Transform,
};

use super::{DAY, FANS, NIGHT, Pal, Pitch, Rgb, Rng, lerp, mix};

/// Pixels per point, as the prototype's `min(2, devicePixelRatio)`.
// ponytail: always 2×; read the window's scale if a 1× screen pays for it.
pub const DPR: f32 = 2.0;

struct Cv {
    pm: Pixmap,
    ts: Transform,
}

fn paint(c: Rgb, a: f32) -> Paint<'static> {
    let ch = |v: f32| v.clamp(0.0, 255.0) / 255.0;
    let mut p = Paint::default();
    p.set_color(
        Color::from_rgba(ch(c[0]), ch(c[1]), ch(c[2]), a.clamp(0.0, 1.0)).unwrap_or(Color::BLACK),
    );
    p.anti_alias = true;
    p
}

fn hex(v: u32) -> Rgb {
    [(v >> 16) as f32, ((v >> 8) & 255) as f32, (v & 255) as f32]
}

fn stroke(w: f32) -> Stroke {
    Stroke {
        width: w,
        ..Stroke::default()
    }
}

/// Points round an arc as line segments: tiny-skia's paths have no arcs.
fn arc(pb: &mut PathBuilder, c: [f32; 2], r: f32, a0: f32, a1: f32, join: bool) {
    let n = ((r * (a1 - a0).abs()) / 2.0).clamp(8.0, 96.0) as usize;
    for k in 0..=n {
        let a = a0 + (a1 - a0) * k as f32 / n as f32;
        let (x, y) = (c[0] + r * a.cos(), c[1] + r * a.sin());
        if k == 0 && !join {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
}

fn circle(c: [f32; 2], r: f32) -> Option<Path> {
    PathBuilder::from_circle(c[0], c[1], r)
}

fn round_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut pb = PathBuilder::new();
    arc(&mut pb, [x + w - r, y + r], r, -PI / 2.0, 0.0, false);
    arc(&mut pb, [x + w - r, y + h - r], r, 0.0, PI / 2.0, true);
    arc(&mut pb, [x + r, y + h - r], r, PI / 2.0, PI, true);
    arc(&mut pb, [x + r, y + r], r, PI, PI * 1.5, true);
    pb.close();
    pb.finish()
}

fn rect_path(x: f32, y: f32, w: f32, h: f32) -> Option<Path> {
    Rect::from_xywh(x, y, w, h).map(PathBuilder::from_rect)
}

impl Cv {
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, p: &Paint) {
        if let Some(r) = Rect::from_xywh(x, y, w, h) {
            self.pm.fill_rect(r, p, self.ts, None);
        }
    }

    fn fill(&mut self, path: Option<Path>, p: &Paint) {
        if let Some(path) = path {
            self.pm
                .fill_path(&path, p, FillRule::Winding, self.ts, None);
        }
    }

    fn stroke(&mut self, path: Option<Path>, p: &Paint, s: &Stroke) {
        if let Some(path) = path {
            self.pm.stroke_path(&path, p, s, self.ts, None);
        }
    }

    fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32, p: &Paint, width: f32) {
        self.stroke(rect_path(x, y, w, h), p, &stroke(width));
    }

    /// A radial glow added onto what is there (canvas `lighter`).
    fn light(&mut self, c: [f32; 2], r: f32, col: Rgb, a: f32, bounds: [f32; 4]) {
        let ch = |v: f32| v / 255.0;
        let stops = vec![
            GradientStop::new(
                0.0,
                Color::from_rgba(ch(col[0]), ch(col[1]), ch(col[2]), a).unwrap_or(Color::BLACK),
            ),
            GradientStop::new(
                1.0,
                Color::from_rgba(ch(col[0]), ch(col[1]), ch(col[2]), 0.0).unwrap_or(Color::BLACK),
            ),
        ];
        let Some(shader) = RadialGradient::new(
            Point::from_xy(c[0], c[1]),
            Point::from_xy(c[0], c[1]),
            r,
            stops,
            SpreadMode::Pad,
            Transform::identity(),
        ) else {
            return;
        };
        let p = Paint {
            shader,
            blend_mode: BlendMode::Plus,
            ..Paint::default()
        };
        let [x, y, w, h] = bounds;
        self.rect(x, y, w, h, &p);
    }
}

/// A radial shadow fading linearly from the centre to the edge, `GLOW` a
/// side: a cloud's shadow, drawn as one image instead of stacked discs.
pub fn glow_tile() -> Vec<u8> {
    let half = GLOW as f32 / 2.0;
    (0..GLOW * GLOW)
        .flat_map(|i| {
            let (x, y) = (
                (i % GLOW) as f32 + 0.5 - half,
                (i / GLOW) as f32 + 0.5 - half,
            );
            let a = (1.0 - x.hypot(y) / half).max(0.0);
            [10, 30, 40, (a * 255.0).round() as u8]
        })
        .collect()
}

pub const GLOW: u32 = 128;

/// The background for `pitch` by day or by night: `w` by `h` straight-alpha
/// RGBA at [`DPR`], or `None` before a layout.
pub fn render(pitch: &Pitch, night: bool) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h) = (pitch.w, pitch.h);
    let pm = Pixmap::new((w * DPR).ceil() as u32, (h * DPR).ceil() as u32)?;
    let base = Transform::from_scale(DPR, DPR);
    let mut c = Cv { pm, ts: base };
    let pal = if night { NIGHT } else { DAY };
    stands(&mut c, pitch, &pal, night);
    let g = pitch.g;
    c.fill(round_rect(g.x, g.y, g.w, g.h, 8.0), &paint(pal.run, 1.0));
    c.stroke(
        round_rect(g.x, g.y, g.w, g.h, 8.0),
        &paint(pal.board, 1.0),
        &stroke(7.0),
    );
    let dash = Stroke {
        width: 2.5,
        dash: StrokeDash::new(vec![22.0, 30.0], 0.0),
        ..Stroke::default()
    };
    let mint = paint([98.0, 230.0, 180.0], if night { 0.85 } else { 0.55 });
    c.stroke(round_rect(g.x, g.y, g.w, g.h, 8.0), &mint, &dash);
    // The pitch, drawn in its own (along, across) coordinates; portrait swaps the axes.
    c.ts = base.pre_translate(pitch.p.x, pitch.p.y);
    if pitch.portrait {
        c.ts =
            c.ts.pre_concat(Transform::from_row(0.0, 1.0, 1.0, 0.0, 0.0, 0.0));
    }
    markings(&mut c, pitch, &pal, night);
    squads(&mut c, pitch, &pal, night);
    c.ts = base;
    floodlights(&mut c, pitch, night);
    let data = c.pm.take();
    Some(((w * DPR).ceil() as u32, (h * DPR).ceil() as u32, data))
}

/// Rings of seats round the ground, every twelfth a concourse, under a roof.
fn stands(c: &mut Cv, pitch: &Pitch, pal: &Pal, night: bool) {
    let (w, h, g) = (pitch.w, pitch.h, pitch.g);
    let r = &mut Rng::new(77 + night as u32);
    c.rect(0.0, 0.0, w, h, &paint(pal.stand, 1.0));
    let (mut d, mut row) = (12.0, 0);
    while d < w.max(h) {
        let (x0, y0, x1, y1) = (g.x - d, g.y - d, g.x + g.w + d, g.y + g.h + d);
        if x0 < -10.0 && y0 < -10.0 && x1 > w + 10.0 && y1 > h + 10.0 {
            break;
        }
        if row == 24 {
            let mut pb = PathBuilder::new();
            if let Some(o) = Rect::from_xywh(-10.0, -10.0, w + 20.0, h + 20.0) {
                pb.push_rect(o);
            }
            if let Some(i) = Rect::from_xywh(x0, y0, x1 - x0, y1 - y0) {
                pb.push_rect(i);
            }
            if let Some(path) = pb.finish() {
                c.pm.fill_path(&path, &paint(pal.roof, 1.0), FillRule::EvenOdd, c.ts, None);
            }
            c.stroke_rect(x0, y0, x1 - x0, y1 - y0, &paint(pal.roof_edge, 1.0), 3.0);
            let edge = paint(pal.roof_edge, 0.35);
            for q in 1..12 {
                let q = q as f32 * 14.0;
                c.stroke_rect(
                    x0 - q,
                    y0 - q,
                    x1 - x0 + q * 2.0,
                    y1 - y0 + q * 2.0,
                    &edge,
                    1.0,
                );
            }
            break;
        }
        if row % 12 == 11 {
            c.stroke_rect(x0, y0, x1 - x0, y1 - y0, &paint(pal.riser, 0.9), 6.0);
        } else {
            c.stroke_rect(
                x0 - 3.5,
                y0 - 3.5,
                x1 - x0 + 7.0,
                y1 - y0 + 7.0,
                &paint(pal.riser, 1.0),
                1.0,
            );
            for [ax, ay, bx, by] in [
                [x0, y0, x1, y0],
                [x0, y1, x1, y1],
                [x0, y0, x0, y1],
                [x1, y0, x1, y1],
            ] {
                let n = ((bx - ax).hypot(by - ay) / 6.0).floor().max(1.0) as usize;
                for i in 0..=n {
                    if i % 22 == 11 {
                        continue; // an aisle
                    }
                    let (x, y) = (
                        lerp(ax, bx, i as f32 / n as f32),
                        lerp(ay, by, i as f32 / n as f32),
                    );
                    if x < -4.0 || y < -4.0 || x > w + 4.0 || y > h + 4.0 {
                        continue;
                    }
                    let seat = if r.next() < 0.84 {
                        let f = FANS[((r.next() * FANS.len() as f32) as usize).min(FANS.len() - 1)];
                        if night && r.next() < 0.03 {
                            paint([220.0, 235.0, 255.0], 0.9)
                        } else {
                            paint(mix(f, pal.stand, 0.42).map(|v| v * pal.fan), 1.0)
                        }
                    } else {
                        paint(pal.seat, 1.0)
                    };
                    c.rect(x - 1.5, y - 1.5, 3.0, 3.0, &seat);
                }
            }
        }
        d += 7.0;
        row += 1;
    }
}

/// Mown stripes, the lines of a 105 by 68 m pitch and both goals.
fn markings(c: &mut Cv, pitch: &Pitch, pal: &Pal, night: bool) {
    let (lp, sp) = (pitch.lp, pitch.sp);
    let n = 14;
    for i in 0..n {
        let col = if i % 2 == 1 { pal.g1 } else { pal.g2 };
        c.rect(
            i as f32 * lp / n as f32,
            0.0,
            lp / n as f32 + 0.5,
            sp,
            &paint(col, 1.0),
        );
    }
    for i in (1..8).step_by(2) {
        c.rect(
            0.0,
            i as f32 * sp / 8.0,
            lp,
            sp / 8.0,
            &paint([255.0; 3], 0.035),
        );
    }
    let (mx, my) = (lp / 105.0, sp / 68.0);
    let (m, cy) = (mx.min(my), sp / 2.0);
    let line = paint(pal.line, if night { 0.75 } else { 0.9 });
    let lw = stroke((0.32 * m).max(1.5));
    c.stroke(rect_path(0.0, 0.0, lp, sp), &line, &lw);
    let mut pb = PathBuilder::new();
    pb.move_to(lp / 2.0, 0.0);
    pb.line_to(lp / 2.0, sp);
    c.stroke(pb.finish(), &line, &lw);
    let mut pb = PathBuilder::new();
    arc(&mut pb, [lp / 2.0, cy], 9.15 * m, 0.0, TAU, false);
    c.stroke(pb.finish(), &line, &lw);
    let white = paint([255.0; 3], 1.0);
    for e in [false, true] {
        let x = |v: f32| if e { lp - v } else { v };
        c.stroke(
            rect_path(
                x(0.0).min(x(16.5 * mx)),
                cy - 20.16 * my,
                16.5 * mx,
                40.32 * my,
            ),
            &line,
            &lw,
        );
        c.stroke(
            rect_path(
                x(0.0).min(x(5.5 * mx)),
                cy - 9.16 * my,
                5.5 * mx,
                18.32 * my,
            ),
            &line,
            &lw,
        );
        let (a0, a1) = if e {
            (PI - 0.93, PI + 0.93)
        } else {
            (-0.93, 0.93)
        };
        let mut pb = PathBuilder::new();
        arc(&mut pb, [x(11.0 * mx), cy], 9.15 * m, a0, a1, false);
        c.stroke(pb.finish(), &line, &lw);
        c.fill(circle([x(11.0 * mx), cy], 1.6), &paint(pal.line, 1.0));
        // The goal and its net.
        let (gw, gd) = (7.32 * my, 2.2 * m);
        let gx = if e { lp } else { -gd };
        let net = if night {
            paint([200.0, 220.0, 230.0], 0.12)
        } else {
            paint([255.0; 3], 0.28)
        };
        c.rect(gx, cy - gw / 2.0, gd, gw, &net);
        let mut pb = PathBuilder::new();
        let mut q = 0.0;
        while q <= gw {
            pb.move_to(gx, cy - gw / 2.0 + q);
            pb.line_to(gx + gd, cy - gw / 2.0 + q);
            q += 3.0;
        }
        q = 0.0;
        while q <= gd {
            pb.move_to(gx + q, cy - gw / 2.0);
            pb.line_to(gx + q, cy + gw / 2.0);
            q += 3.0;
        }
        c.stroke(pb.finish(), &paint([255.0; 3], 0.45), &stroke(0.6));
        c.stroke(rect_path(gx, cy - gw / 2.0, gd, gw), &white, &stroke(2.6));
    }
    c.fill(circle([lp / 2.0, cy], 2.0), &paint(pal.line, 1.0));
    for sy in [0.0, sp] {
        for sx in [0.0, lp] {
            c.stroke(circle([sx, sy], 1.2 * m), &line, &lw);
        }
    }
}

/// Each squad's divider discs, drill cones and dugout.
fn squads(c: &mut Cv, pitch: &Pitch, pal: &Pal, night: bool) {
    let (k, t, sp) = (pitch.k, pitch.strip, pitch.sp);
    for z in &pitch.zones {
        if z.i > 0 {
            let mut s = 10.0;
            while s < sp {
                c.fill(
                    circle([z.z0, s], 2.6 * k),
                    &paint([240.0, 248.0, 255.0], 0.85),
                );
                s += 22.0;
            }
        }
        for q in &z.cones {
            c.fill(
                circle([q.l + 1.5, q.s + 1.5], 3.6 * k),
                &paint([0.0; 3], 0.18),
            );
            c.fill(
                circle([q.l, q.s], 3.6 * k),
                &paint(q.c.map(|v| v * if night { 0.75 } else { 1.0 }), 1.0),
            );
            c.fill(circle([q.l, q.s], 1.2 * k), &paint([255.0; 3], 0.7));
        }
        let cx = z.z0 + z.zw * 0.3;
        let len = (z.zw * 0.5).min(30.0 + 14.0 * k * 5.0);
        let x0 = cx - len / 2.0;
        c.fill(
            round_rect(x0 + 2.0, -t * 0.94 + 2.0, len, t * 0.6, 5.0),
            &paint([0.0; 3], 0.2),
        );
        c.fill(
            round_rect(x0, -t * 0.94, len, t * 0.6, 5.0),
            &paint(pal.dug, 0.92),
        );
        c.rect(x0, -t * 0.94, len, t * 0.12, &paint(pal.board, 0.6));
        let seat = paint(pal.seat, 1.0);
        let mut q = x0 + 8.0;
        while q < cx + len / 2.0 - 4.0 {
            c.rect(q - 3.0 * k, -t * 0.66, 6.0 * k, 5.0 * k, &seat);
            q += 9.0 * k;
        }
    }
}

/// The four towers, and at night their pools of light on the grass.
fn floodlights(c: &mut Cv, pitch: &Pitch, night: bool) {
    let (w, h, p) = (pitch.w, pitch.h, pitch.p);
    let mid = [p.x + p.w / 2.0, p.y + p.h / 2.0];
    if night {
        let r = p.w.max(p.h) * 0.85;
        for t in &pitch.towers {
            let at = [lerp(t[0], mid[0], 0.42), lerp(t[1], mid[1], 0.42)];
            c.light(at, r, [120.0, 170.0, 140.0], 0.26, [0.0, 0.0, w, h]);
        }
    }
    let base = c.ts;
    for t in &pitch.towers {
        let ang = (mid[1] - t[1]).atan2(mid[0] - t[0]);
        c.ts = base
            .pre_translate(t[0], t[1])
            .pre_rotate((ang + PI / 2.0).to_degrees());
        let (frame, lamp) = if night {
            (hex(0x1a1f2a), hex(0xfbfff4))
        } else {
            (hex(0x5c6472), hex(0xd6dde6))
        };
        c.rect(-11.0, -6.0, 22.0, 11.0, &paint(frame, 1.0));
        for i in 0..4 {
            for j in 0..2 {
                c.rect(
                    -9.5 + i as f32 * 5.0,
                    -4.5 + j as f32 * 4.5,
                    4.0,
                    3.5,
                    &paint(lamp, 1.0),
                );
            }
        }
        c.ts = base;
        if night {
            c.light(
                *t,
                46.0,
                [255.0, 255.0, 240.0],
                0.75,
                [t[0] - 46.0, t[1] - 46.0, 92.0, 92.0],
            );
        }
    }
}
