//! The reef: the fleet as an aquarium, each machine a reef with its buoy,
//! every machine's agents it can reach as fish of their harness's species
//! acting out their state. A press on a fish selects it and the camera follows it
//! in; a press on the selected one (or Return) shows its terminal, as the
//! galaxy does. Escape lets go, then closes. Night follows a dark theme.
//!
//! Drawn back to front as the prototype: scenery under the sand's caustics
//! in one canvas, the caustics as a scrolling tile, the rest in another,
//! then labels, the fish's press targets and the readout.

use rune::{Cx, El, canvas, el, row, text};

use super::playground::{self, AMBER, ON_AMBER, Who, badge, stage, target};
use super::{CAPTION, Ink};
use crate::creature;
use crate::desk::Desk;
use crate::reef::{Pt, Reef, TILE};

pub(super) mod drift;
mod fish;
mod forms;
pub(super) mod pen;
mod scene;

use pen::Pen;
pub(super) use pen::rgba;

/// A pill over the scene, at a world point.
pub(super) struct Label {
    pub at: Pt,
    pub text: String,
    /// The amber count a buoy shows of agents needing you.
    pub badge: Option<usize>,
    pub amber: bool,
    /// A buoy's machine, whose colour leads the pill.
    pub host: Option<String>,
}

pub(super) fn view(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> El {
    let r = &desk.reef.scene;
    let (root, waiting) = stage(&desk.reef, "reef", "Reef", r.zones.len(), p, cx);
    if r.zones.is_empty() {
        return root.child(readout(r, waiting, p));
    }
    let pal = r.pal();

    let mut back = Pen::new();
    back.set(r, 1.0);
    scene::water(&mut back, r, &pal);
    back.set(r, 0.45);
    scene::far(&mut back, r, &pal);
    back.set(r, 1.0);
    scene::rays(&mut back, r);
    scene::sky(&mut back, r, &pal);
    for k in r.kelp.iter().filter(|k| k.back) {
        scene::kelp(&mut back, r, &pal, k, 0.7);
    }
    scene::sand(&mut back, r, &pal);

    let mut front = Pen::new();
    front.set(r, 1.0);
    let mut labels: Vec<Label> = r
        .zones
        .iter()
        .enumerate()
        .map(|(i, z)| scene::buoy(&mut front, r, z, i))
        .collect();
    for k in r.kelp.iter().filter(|k| !k.back && !k.front) {
        scene::kelp(&mut front, r, &pal, k, 1.0);
    }
    for z in &r.zones {
        scene::reef_back(&mut front, r, &pal, z);
    }
    for z in &r.zones {
        scene::reef_front(&mut front, r, &pal, z);
    }
    drift::sparks(&mut front, r);
    for f in &r.fish {
        fish::fish(&mut front, r, &pal, f, &mut labels);
    }
    drift::bubbles(&mut front, r);
    drift::snow(&mut front, r);
    for k in r.kelp.iter().filter(|k| k.front) {
        scene::kelp(&mut front, r, &pal, k, 1.0);
    }
    drift::vignette(&mut front, r);

    let to = r.to_screen(1.0);
    let targets = r.fish.iter().map(|f| {
        let at = to([f.x, f.y]);
        let rad = (f.len(r.s) * 0.75 + 6.0) * r.cam.z;
        target::<Reef>("reef-fish", &who(r, f), at, rad)
    });

    root.child(
        canvas(back.into_shape())
            .key("reef-back")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full(),
    )
    .children(caustics(desk, r))
    .child(
        canvas(front.into_shape())
            .key("reef-front")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .on_press(Desk::scene_deselect::<Reef>),
    )
    .children(labels.iter().map(|l| pill(l, to(l.at), r.w)))
    .children(targets)
    .child(readout(r, waiting, p))
}

/// The prototype's two scrolling layers of caustic light on the sand, cut
/// at the sand's lowest dip.
fn caustics(desk: &Desk, r: &Reef) -> Vec<El> {
    let to = r.to_screen(1.0);
    let z = r.cam.z;
    let top = to([0.0, r.floor_y + 10.5 * r.s])[1].max(0.0);
    let h = r.h - top;
    if h <= 0.0 {
        return Vec::new();
    }
    let tile = TILE as f32;
    let base = r.floor_y - 12.0 * r.s;
    [(13.0, 6.0, 1.0, 0.3), (-9.0, 10.0, 1.37, 0.2)]
        .into_iter()
        .map(|(vx, vy, scale, alpha): (f32, f32, f32, f32)| {
            let (sx, sy) = (scale * r.s, scale * r.s * 0.42);
            let (tw, th) = (tile * sx * z, tile * sy * z);
            let o = to([((r.t * vx) % tile) * sx, base + ((r.t * vy) % tile) * sy]);
            let (x0, y0) = (o[0].rem_euclid(tw) - tw, (o[1] - top).rem_euclid(th) - th);
            let mut tiles = Vec::new();
            let mut y = y0;
            while y < h {
                let mut x = x0;
                while x < r.w {
                    tiles.push(el().absolute(x, y).size(tw, th).pixels(&desk.reef.paint));
                    x += tw;
                }
                y += th;
            }
            el().absolute(0.0, top)
                .w(r.w)
                .h(h)
                .clip_rounded(0.0)
                .opacity(alpha * (1.0 - r.nt * 0.9))
                .children(tiles)
        })
        .collect()
}

/// A label pill centred on `at`, kept inside the view.
pub(super) fn pill(l: &Label, at: Pt, w: f32) -> El {
    // ponytail: an estimated width keeps a pill inside the view; measure the
    // text if pills ever clip at the edges.
    let est =
        l.text.chars().count() as f32 * 6.6 + 14.0 + if l.badge.is_some() { 18.0 } else { 0.0 };
    let x = if w > est + 12.0 {
        at[0].clamp(6.0 + est / 2.0, w - 6.0 - est / 2.0)
    } else {
        at[0]
    };
    let (fill, ink) = if l.amber {
        (rgba(AMBER, 0.95), rgba(ON_AMBER, 1.0))
    } else {
        (
            rgba([4.0, 20.0, 34.0], 0.72),
            rgba([228.0, 245.0, 247.0], 1.0),
        )
    };
    let host = (l.host.as_deref()).map(|host| crate::host_color::dot(host, 7.0, fill.alpha(1.0)));
    row()
        .absolute(x - 160.0, at[1] - 9.5)
        .w(320.0)
        .justify_center()
        .child(
            row()
                .h(19.0)
                .px(7.0)
                .gap(5.0)
                .radius(9.5)
                .items_center()
                .fill(fill)
                .children(host)
                .child(text(l.text.clone()).font(CAPTION).color(ink))
                .children(l.badge.map(|n| badge(n, 14.0, CAPTION))),
        )
}

fn who<'a>(r: &'a Reef, f: &'a crate::reef::Fish) -> Who<'a> {
    Who {
        id: &f.who.id,
        name: &f.who.name,
        mood: f.who.mood,
        host: &r.zone_of(f).host,
    }
}

fn readout(r: &Reef, waiting: usize, p: &Ink) -> El {
    // Its form and work, when its machine publishes agent stats.
    let picked = r.selected_fish().map(|f| {
        let card = f.who.work.map(|w| {
            let form = creature::form_name(f.tier, f.who.species.form);
            format!("Form: {form} · Work: {}", creature::work_line(&w))
        });
        (who(r, f), card)
    });
    // Along the foot once laid out, clear of the buoys' labels on the surface.
    playground::readout("reef", r, [r.zones.len(), waiting], r.h, picked, p)
}
