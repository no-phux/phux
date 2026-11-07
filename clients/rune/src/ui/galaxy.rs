//! The galaxy: the fleet as a core, each machine a planet round it, and the
//! connected machine's agents on rings round it, coloured by their marks.
//! Related agents (one session, or one project) are joined by a bowed beam
//! with a light running along it. Drag pans, the wheel pans (zooms with ⌘ or
//! Ctrl), a pinch zooms, a double click fits it all again. A press on an
//! agent shows its terminal; one on a planet flies to it.

use std::f32::consts::TAU;

use rune::{Color, Cx, El, LineCap, Path, Shape, canvas, col, el, row, text};

use super::sidebar::mark_color;
use super::{CAPTION, Ink, LABEL, LABEL_STRONG, TITLEBAR_H, overlay, pressable};
use crate::activity::Mark;
use crate::desk::Desk;
use crate::galaxy::{self, Camera, Kin, Pt, System};

/// Below this scale the agents' names would pile up, so only planets are named.
const NAMES_FROM: f32 = 0.6;
const STARS: usize = 140;

pub(super) fn view(desk: &Desk, p: &Ink, cx: &mut Cx<Desk>) -> El {
    desk.galaxy.still.set(cx.is_reduced_motion());
    let planets = desk.planets();
    let plan = Desk::galaxy_plan(&planets);
    let camera = desk.galaxy.camera;
    let time = desk.galaxy_time();
    let waiting = planets
        .iter()
        .flat_map(|planet| &planet.stars)
        .filter(|star| star.mark.needs_you())
        .count();
    let light = if waiting > 0 { p.marks.alert } else { p.accent };
    let sky = Sky {
        camera,
        shape: Shape::new(),
    };

    // Where every agent is this frame, with its kin for the beams.
    let placed: Vec<(usize, usize, Pt)> =
        planets
            .iter()
            .zip(&plan.systems)
            .enumerate()
            .flat_map(|(at, (planet, system))| {
                planet.stars.iter().enumerate().map(move |(i, star)| {
                    (at, i, system.point(i, time, star.mark == Mark::Working))
                })
            })
            .collect();
    let kin: Vec<Kin<'_>> = placed
        .iter()
        .map(|&(at, i, _)| {
            let star = &planets[at].stars[i];
            Kin {
                host: &planets[at].name,
                session: &star.session,
                project: star.cwd.as_deref().and_then(galaxy::project),
            }
        })
        .collect();

    let mut sky = sky.backdrop(plan.side, time, p);
    if !plan.systems.is_empty() {
        sky = sky.dashed(plan.core, plan.orbit, 96, 0.0, 1.0, p.separator);
    }
    for (planet, system) in planets.iter().zip(&plan.systems) {
        let beam = light.alpha(if planet.online { 0.35 } else { 0.12 });
        sky = sky.line(plan.core, system.centre, 1.5, beam);
        for ring in 0..system.rings().min(system.count) {
            let r = System::ring_radius(ring);
            sky = sky.dashed(system.centre, r, 48, 0.0, 1.0, p.separator.alpha(0.6));
        }
    }
    for (a, b) in galaxy::related(&kin) {
        sky = sky.kin(placed[a].2, placed[b].2, plan.core, time, p.accent);
    }
    for &(at, i, point) in &placed {
        let star = &planets[at].stars[i];
        let color = mark_color(star.mark, p);
        let centre = plan.systems[at].centre;
        sky = sky.line(centre, point, 1.0, color.alpha(0.45));
        if star.mark.needs_you() {
            // A beacon: a ring that swells and fades, once a second and a half.
            let swell = (time % 1.5) / 1.5;
            sky = sky.ring(
                point,
                18.0 + 22.0 * swell,
                2.0,
                color.alpha(0.6 * (1.0 - swell)),
            );
        }
        let picked = desk.galaxy.picked.as_ref() == Some(&star.terminal);
        if picked {
            sky = sky.ring(point, 16.0, 2.0, p.text);
        }
        sky = sky
            .disc(point, 12.0, color.alpha(0.25))
            .disc(point, 6.0, color);
    }
    for (planet, system) in planets.iter().zip(&plan.systems) {
        let edge = if planet.online { light } else { p.dim };
        sky = sky
            .disc(system.centre, 20.0, p.raised)
            .ring(system.centre, 20.0, 2.0, edge)
            .disc(system.centre, 6.0, edge);
    }
    let pulse = 0.5 + 0.5 * (time * 2.0).sin();
    sky = sky
        .disc(plan.core, 30.0 + 4.0 * pulse, light.alpha(0.15))
        .disc(plan.core, 14.0, light)
        .dashed(plan.core, 46.0, 18, time * 0.25, 0.6, light.alpha(0.7))
        .dashed(plan.core, 60.0, 48, -time * 0.12, 0.2, light.alpha(0.4));

    let mut labels: Vec<El> = Vec::new();
    for (planet, system) in planets.iter().zip(&plan.systems) {
        let centre = system.centre;
        let at = camera.screen(centre);
        let note = planet
            .note
            .clone()
            .or((!planet.online).then(|| "offline".to_owned()));
        labels.push(
            pressable(format!("galaxy-planet-{}", planet.name), p)
                .absolute(at[0] - 70.0, at[1] - 22.0)
                .w(140.0)
                .radius(8.0)
                .pt(46.0)
                .pb(4.0)
                .label(planet.name.clone())
                .on_press(move |desk: &mut Desk| desk.fly_to(centre))
                .child(
                    col()
                        .w_full()
                        .items_center()
                        .child(text(planet.name.clone()).font(LABEL_STRONG).color(p.text))
                        .children(note.map(|n| text(n).font(CAPTION).color(p.dim))),
                ),
        );
    }
    for &(at, i, point) in &placed {
        let star = &planets[at].stars[i];
        let s = camera.screen(point);
        let (terminal, session) = (star.terminal.clone(), star.session.clone());
        let named = camera.scale >= NAMES_FROM;
        let word = word(star.mark);
        labels.push(
            pressable(format!("galaxy-star-{:?}", star.terminal), p)
                .absolute(s[0] - 60.0, s[1] - 14.0)
                .w(120.0)
                .radius(8.0)
                .pt(if named { 24.0 } else { 28.0 })
                .label(format!("{}, {word}", star.name))
                .on_press(move |desk: &mut Desk| desk.open_star(terminal.clone(), &session))
                .children(named.then(|| {
                    col()
                        .w_full()
                        .items_center()
                        .child(text(star.name.clone()).font(LABEL).color(p.text))
                        .child(text(word).font(CAPTION).color(mark_color(star.mark, p)))
                })),
        );
    }

    let hosts = planets.len();
    let agents = placed.len();
    let working = planets
        .iter()
        .flat_map(|planet| &planet.stars)
        .filter(|star| star.mark == Mark::Working)
        .count();
    let readout = row()
        .absolute(16.0, TITLEBAR_H + 4.0)
        .gap(10.0)
        .items_center()
        .child(
            el().px(10.0)
                .py(4.0)
                .radius(10.0)
                .fill(p.raised.alpha(0.85))
                .child(
                    text(format!(
                        "{hosts} hosts · {agents} agents · {working} working"
                    ))
                    .font(CAPTION)
                    .color(p.dim),
                ),
        )
        .children((waiting > 0).then(|| {
            pressable("galaxy-next-waiting", p)
                .px(12.0)
                .py(5.0)
                .radius(12.0)
                .fill(p.marks.alert)
                .label("Show the next agent that needs you")
                .on_press(Desk::next_waiting)
                .child(
                    text(format!("{waiting} need you"))
                        .font(LABEL_STRONG)
                        .color(crate::theme::on_fill(p.marks.alert)),
                )
        }))
        .child(overlay::icon_button(
            "✕",
            "Close",
            "galaxy-close",
            p,
            Desk::close_galaxy,
        ));

    el().key("galaxy")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .fill(p.bg)
        .modal(false, Desk::close_galaxy)
        .label("Galaxy")
        .on_layout(|desk: &mut Desk, r| desk.galaxy_laid_out(r[2], r[3]))
        .child(
            canvas(sky.shape)
                .key("galaxy-sky")
                .absolute(0.0, 0.0)
                .w_full()
                .h_full()
                .on_drag(|desk: &mut Desk, drag, _| desk.galaxy_drag(drag))
                .on_double_press(Desk::fit_galaxy)
                .on_wheel(|desk: &mut Desk, e, _| {
                    let camera = &mut desk.galaxy.camera;
                    if e.mods.cmd || e.mods.ctrl {
                        camera.zoom((1.0 + e.dy * 0.01).max(0.1), [e.x, e.y]);
                    } else {
                        camera.offset = [camera.offset[0] + e.dx, camera.offset[1] + e.dy];
                    }
                    true
                })
                .on_magnify(|desk: &mut Desk, e, _| {
                    desk.galaxy.camera.zoom(1.0 + e.delta, [e.x, e.y]);
                    true
                }),
        )
        .children(labels)
        .child(readout)
}

/// What a person reads for the mark.
fn word(mark: Mark) -> &'static str {
    match mark {
        Mark::Blocked => "needs you",
        Mark::Done => "finished",
        Mark::Failed => "failed",
        Mark::Bell => "rang",
        Mark::Working => "working",
        Mark::Running => "running",
        Mark::Background => "background",
        Mark::Succeeded => "succeeded",
        Mark::Idle | Mark::None => "idle",
    }
}

/// The drawing, in world points carried to the screen by the camera.
struct Sky {
    camera: Camera,
    shape: Shape,
}

impl Sky {
    fn at(&self, world: Pt) -> [f32; 2] {
        self.camera.screen(world)
    }

    fn circle(&self, centre: Pt, r: f32, from: f32, to: f32, n: usize) -> Vec<[f32; 2]> {
        let c = self.at(centre);
        let r = r * self.camera.scale;
        (0..=n)
            .map(|k| {
                let a = from + (to - from) * k as f32 / n as f32;
                [c[0] + r * a.cos(), c[1] + r * a.sin()]
            })
            .collect()
    }

    /// Stars that twinkle, each at its own place in the world.
    fn backdrop(mut self, side: f32, time: f32, p: &Ink) -> Sky {
        for i in 0..STARS {
            // The R2 low-discrepancy sequence: spread out, the same every frame.
            let x = (0.5 + i as f32 * 0.754_877_7).fract();
            let y = (0.5 + i as f32 * 0.569_840_3).fract();
            let twinkle = 0.25 + 0.35 * (1.0 + (time * (0.6 + y) + x * 40.0).sin()) / 2.0;
            let dot = self.circle([x * side, y * side], 0.8 + y, 0.0, TAU, 6);
            self.shape = self.shape.fill(dot, p.dim.alpha(twinkle));
        }
        self
    }

    fn line(mut self, a: Pt, b: Pt, width: f32, color: Color) -> Sky {
        let (a, b) = (self.at(a), self.at(b));
        self.shape = self.shape.line(a, b, width, LineCap::Round, color);
        self
    }

    fn ring(mut self, centre: Pt, r: f32, width: f32, color: Color) -> Sky {
        let points = self.circle(centre, r, 0.0, TAU, 40);
        self.shape = self
            .shape
            .stroke_with(points, true, width, LineCap::Round, color);
        self
    }

    fn disc(mut self, centre: Pt, r: f32, color: Color) -> Sky {
        let mut points = self.circle(centre, r, 0.0, TAU, 32);
        points.pop();
        self.shape = self.shape.fill(points, color);
        self
    }

    /// `dashes` dashes round a circle, each `share` of its step long, turned by `phase` turns.
    fn dashed(
        mut self,
        centre: Pt,
        r: f32,
        dashes: usize,
        phase: f32,
        share: f32,
        color: Color,
    ) -> Sky {
        let step = TAU / dashes as f32;
        for k in 0..dashes {
            let from = phase * TAU + k as f32 * step;
            let points = self.circle(centre, r, from, from + step * share * 0.5, 3);
            self.shape = self.shape.stroke(points, 1.0, color);
        }
        self
    }

    /// Two related agents: a beam bowed away from the core, and a light
    /// running along it once every three seconds.
    fn kin(mut self, a: Pt, b: Pt, core: Pt, time: f32, color: Color) -> Sky {
        let c = galaxy::bow(a, b, core);
        let path = Path::new()
            .move_to(self.at(a))
            .quad_to(self.at(c), self.at(b));
        self.shape = self
            .shape
            .stroke_path(&path, 1.5, LineCap::Round, color.alpha(0.45));
        let t = (time / 3.0).fract();
        let glow = galaxy::along(a, c, b, t);
        self.disc(glow, 7.0, color.alpha(0.3))
            .disc(glow, 3.0, color)
    }
}
