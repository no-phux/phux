//! Kelp and the sand with its scrolling caustics.

use super::*;

pub(super) fn kelp(pen: &mut Pen, sim: &Sim, pal: &Pal, k: &super::super::reef::Kelp, alpha: f32) {
    let (s, t) = (sim.s, sim.t);
    let n: usize = 16;
    let base = sim.sand_y(k.x) + 6. * s;
    let pts = (0..=n)
        .map(|i| {
            let f = i as f32 / n as f32;
            (
                k.x + (t * 0.85 + k.seed + i as f32 * 0.3).sin() * f.powf(1.3) * k.h * 0.09
                    + (t * 0.3 + k.seed).sin() * f * 10. * s,
                base - k.h * f,
            )
        })
        .collect::<Vec<_>>();
    let c = if k.back {
        mix(pal.kelp, pal.w1, 0.5)
    } else {
        pal.kelp
    };
    // The stem: one tapering ribbon.
    let mut left = Vec::new();
    let mut right = Vec::new();
    for i in 0..=n {
        let (a, b) = (pts[i.saturating_sub(1)], pts[(i + 1).min(n)]);
        let ang = (b.1 - a.1).atan2(b.0 - a.0);
        let half = k.w * (1. - i as f32 / n as f32 * 0.6) / 2.;
        let (nx, ny) = (-ang.sin() * half, ang.cos() * half);
        left.push((pts[i].0 + nx, pts[i].1 + ny));
        right.push((pts[i].0 - nx, pts[i].1 - ny));
    }
    right.reverse();
    left.extend(right);
    let leaves = (2..n)
        .map(|i| {
            let ((x, y), (px0, py0)) = (pts[i], pts[i - 1]);
            let side = if i % 2 == 1 { 1. } else { -1. };
            let a =
                (y - py0).atan2(x - px0) + side * (1. + (t * 1.2 + i as f32 + k.seed).sin() * 0.15);
            let len = 17. * s * (1. - i as f32 / n as f32 * 0.4);
            let (sin, cos) = a.sin_cos();
            arc(
                x + cos * len * 0.5,
                y + sin * len * 0.5,
                len * 0.55,
                len * 0.2,
                a,
                0.,
                TAU,
                12,
            )
        })
        .collect::<Vec<_>>();
    pen.fill(&left, rgba(mix(c, BLACK, 0.25), alpha));
    pen.fill_many(&leaves, rgba(c, alpha * 0.92));
}

/// The caustic tile: Worley F2−F1, its bright cell edges reading as light on
/// the sand. Built once; one image per (quantised) opacity, since an image
/// paints at full strength.
pub(super) fn caustic(level: u8) -> Option<Arc<RenderImage>> {
    const N: usize = 256;
    static BASE: OnceLock<Vec<f32>> = OnceLock::new();
    static TILES: Mutex<Option<HashMap<u8, Arc<RenderImage>>>> = Mutex::new(None);
    let base = BASE.get_or_init(|| {
        let mut r = Rng::new(7);
        let pts = (0..24)
            .map(|_| (r.next() * N as f32, r.next() * N as f32))
            .collect::<Vec<_>>();
        let mut out = vec![0.; N * N];
        for y in 0..N {
            for x in 0..N {
                let (mut f1, mut f2) = (f32::MAX, f32::MAX);
                for &(px0, py0) in &pts {
                    let mut dx = (x as f32 - px0).abs();
                    let mut dy = (y as f32 - py0).abs();
                    dx = dx.min(N as f32 - dx);
                    dy = dy.min(N as f32 - dy);
                    let d = dx * dx + dy * dy;
                    if d < f1 {
                        f2 = f1;
                        f1 = d;
                    } else if d < f2 {
                        f2 = d;
                    }
                }
                out[y * N + x] = (1. - (f2.sqrt() - f1.sqrt()) / 9.).clamp(0., 1.).powf(2.4);
            }
        }
        out
    });
    let mut tiles = TILES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tiles = tiles.get_or_insert_with(HashMap::new);
    if let Some(tile) = tiles.get(&level) {
        return Some(Arc::clone(tile));
    }
    // ponytail: one tile per opacity step (at most ~16 live); a shader would
    // take the opacity directly if gpui grows image tinting.
    let alpha = f32::from(level) / 64.;
    let mut buffer = image::RgbaImage::new(N as u32, N as u32);
    for (i, pixel) in buffer.pixels_mut().enumerate() {
        // GPUI's atlas takes straight-alpha BGRA.
        *pixel = image::Rgba([225, 250, 255, (base[i] * alpha * 255.) as u8]);
    }
    let tile = Arc::new(RenderImage::new([image::Frame::new(buffer)]));
    tiles.insert(level, Arc::clone(&tile));
    Some(tile)
}

pub(super) fn sand(pen: &mut Pen, sim: &Sim, pal: &Pal) {
    let (w, h, s, nt, t, floor) = (sim.w, sim.h, sim.s, sim.nt, sim.t, sim.floor_y);
    let top = floor - 12. * s;
    let mut ground = Vec::new();
    let mut x = -20.;
    while x <= w + 20. {
        ground.push((x, sim.sand_y(x)));
        x += 8.;
    }
    ground.push((w + 20., h + 40.));
    ground.push((-20., h + 40.));
    pen.fill(
        &ground,
        grad(
            180.,
            rgba(mix(pal.sand0, pal.w1, 0.18), 1.),
            0.,
            rgba(pal.sand1, 1.),
            (h - top) / (h + 40. - top),
        ),
    );
    let rows = (1..6)
        .map(|row| {
            let yy = floor + row as f32 * (h - floor) / 6.;
            let mut line = Vec::new();
            let mut x = -20.;
            while x <= w + 20. {
                line.push((x, yy + (x * 0.05 + row as f32 * 1.7).sin() * 2. * s));
                x += 12.;
            }
            line
        })
        .collect::<Vec<_>>();
    pen.stroke_many(&rows, false, s, rgba([255., 250., 230.], 0.12 - nt * 0.08));
    let mut shades: [Vec<Vec<P>>; 3] = Default::default();
    for p in &sim.pebbles {
        let y = sim.sand_y(p.x) + 6. * s + p.dy * (h - floor) * 0.8;
        shades[((p.sh * 3.) as usize).min(2)].push(arc(p.x, y, p.rx, p.rx * 0.6, 0., 0., TAU, 10));
    }
    for (i, polys) in shades.iter().enumerate() {
        let sh = (i as f32 + 0.5) / 3.;
        pen.fill_many(polys, rgba(mix(pal.sand1, [60., 50., 40.], sh * 0.4), 0.7));
    }
    // Two scrolling caustic layers, in narrow strips so none rises above
    // the sand's wavy edge.
    let k = pen.k();
    let left = sim.cam.x - w / (2. * sim.cam.z) - 24.;
    let right = sim.cam.x + w / (2. * sim.cam.z) + 24.;
    for (sx, sy, ls, a) in [(13., 6., 1., 0.3), (-9., 10., 1.37, 0.2)] {
        let alpha = a * (1. - nt * 0.9);
        let level = (alpha * 64.).round().clamp(0., 64.) as u8;
        if level == 0 {
            continue;
        }
        let Some(tile) = caustic(level) else { continue };
        let (tw, th) = (256. * ls * s, 256. * ls * s * 0.42);
        let ox = ((t * sx) % 256.) * ls * s;
        let oy = ((t * sy) % 256.) * ls * s * 0.42;
        let strip = 24.;
        let mut x0 = (left / strip).floor() * strip;
        while x0 < right {
            let edge = [x0, x0 + strip / 2., x0 + strip]
                .map(|x| sim.sand_y(x))
                .into_iter()
                .fold(f32::MIN, f32::max);
            let clip = rect(pen, (x0, edge), (x0 + strip, h + 40.));
            let i0 = ((x0 - ox) / tw).floor() as i32;
            let i1 = ((x0 + strip - ox) / tw).floor() as i32;
            let j0 = ((edge - top - oy) / th).floor() as i32;
            let j1 = ((h + 40. - top - oy) / th).floor() as i32;
            let tile = &tile;
            pen.clipped(clip, |pen| {
                for i in i0..=i1 {
                    for j in j0..=j1 {
                        let at = pen.xy((ox + i as f32 * tw, top + oy + j as f32 * th));
                        let cell = Bounds {
                            origin: point(px(at.0), px(at.1)),
                            size: size(px(tw * k), px(th * k)),
                        };
                        let _ = pen.win.paint_image(
                            cell,
                            cell,
                            Corners::default(),
                            Arc::clone(tile),
                            0,
                            false,
                        );
                    }
                }
            });
            x0 += strip;
        }
    }
}
