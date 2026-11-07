//! The galaxy map: the fleet as a command center under the playground's
//! camera. The readout counts what is working and flies to each agent that
//! needs you in turn; a planet click flies into that machine's system; Fit
//! shows the whole galaxy again. Related agents are joined by arcs with a
//! pulse running along them; on the matrix map the same links carry packets.

use gpui::prelude::*;
use gpui::{AnyElement, Bounds, ContentMask, Context, Hsla, Pixels, Point, canvas, div, point, px};

use super::super::cards::{state_color, tile_id};
use super::galaxy::{self, Galaxy, RING_GAP, SHELL};
use super::plan::{self, Pt};
use super::{Camera, Ghost, Paint, ellipse, fill, label, now_ms, stage, target};
use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::fleet::Agent;
use crate::app::skin::Look;

/// One agent's beam from its planet, in its state's colour.
struct Beam {
    from: Pt,
    to: Pt,
    color: Hsla,
    waiting: bool,
}

/// The galaxy's planets, beams and links at this moment, ready to paint.
struct Sky {
    core: Pt,
    orbit: f32,
    planets: Vec<(Pt, usize, bool)>,
    beams: Vec<Beam>,
    links: Vec<(Pt, Pt)>,
    /// The theme's light, or the needs-you colour while anything waits.
    light: Hsla,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn scene(
    desk: &mut Desk,
    hosts: Vec<(Option<String>, bool)>,
    agents: &[Agent],
    (width, height): (f32, f32),
    time: f32,
    paint: Paint,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let galaxy = Galaxy::new(hosts, agents);
    let view = &mut desk.fleet_view.playground;
    if !view.camera.placed {
        let scale = galaxy.fit(width, height);
        view.camera.look_at(galaxy.core, scale, width, height);
    }
    let camera = view.camera;
    let now_ms = now_ms();
    let mut ghosts = Vec::new();
    let mut beams = Vec::new();
    // Waiting agents hold still, so where they rest is where they are.
    let mut waiting = Vec::new();
    for system in &galaxy.systems {
        for (index, agent) in system.agents.iter().enumerate() {
            let foot = system.point(index, time, agent.info.working());
            let color = state_color(&agent.info, look);
            if agent.info.needs_you() {
                let id = tile_id(agent.destination.host.as_deref(), &agent.terminal);
                waiting.push((id, foot, color));
            }
            beams.push(Beam {
                from: system.centre,
                to: foot,
                color,
                waiting: agent.info.needs_you(),
            });
            ghosts.push(Ghost {
                agent: agent.clone(),
                foot,
                strip: plan::strip(&agent.info, now_ms),
                depth: foot.y,
            });
        }
    }
    let related = ghosts.iter().map(|g| g.agent.clone()).collect::<Vec<_>>();
    let sky = Sky {
        core: galaxy.core,
        orbit: galaxy.orbit,
        planets: galaxy
            .systems
            .iter()
            .map(|s| (s.centre, s.rings(), s.online))
            .collect(),
        links: galaxy::relations(&related)
            .into_iter()
            .map(|(a, b)| (ghosts[a].foot, ghosts[b].foot))
            .collect(),
        light: waiting.first().map_or(paint.edge, |(_, _, color)| *color),
        beams,
    };
    let sprites = ghosts
        .iter()
        .map(|ghost| (ghost.foot, ghost.strip, ghost.depth))
        .collect::<Vec<_>>();
    let labels = ghosts
        .iter()
        .map(|ghost| label(ghost, &camera, look))
        .collect::<Vec<_>>();
    let targets = ghosts
        .iter()
        .map(|ghost| target(ghost, &camera, cx))
        .collect::<Vec<_>>();
    let planets = galaxy
        .systems
        .iter()
        .map(|system| planet(system, &camera, (width, height), look, cx))
        .collect::<Vec<_>>();
    let readout = readout(&galaxy, agents, waiting, (width, height), look, cx);
    stage(width, height, look, cx)
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    window.with_content_mask(Some(ContentMask { bounds }), |window| {
                        paint.sky(&sky, &camera, bounds, time, window);
                        paint.ghosts(&sprites, &camera, bounds, time, window);
                    });
                },
            )
            .absolute()
            .size_full(),
        )
        .children(labels)
        .children(planets)
        .children(targets)
        .child(readout)
        .into_any_element()
}

/// A machine's name under its planet; a click flies into its system.
fn planet(
    system: &galaxy::System,
    camera: &Camera,
    (width, height): (f32, f32),
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let at = camera.screen(system.centre);
    let name = system.host.clone().unwrap_or_else(|| "Local".into());
    let selector = format!("fleet-galaxy-planet-{name}");
    let centre = system.centre;
    div()
        .id(selector.clone())
        .debug_selector(move || selector.clone())
        .absolute()
        .left(px(at.x - 60.))
        .top(px(at.y - 24.))
        .w(px(120.))
        .pt(px(48.))
        .flex()
        .flex_col()
        .items_center()
        .cursor_pointer()
        .text_size(px(12.))
        .text_color(look.ink)
        .child(name)
        .when(!system.online, |label| {
            label.child(
                div()
                    .text_size(px(10.))
                    .text_color(from_hex(&look.chrome.muted))
                    .child("offline"),
            )
        })
        .on_click(cx.listener(move |desk, _, _, cx| {
            desk.fleet_view
                .playground
                .camera
                .look_at(centre, 1., width, height);
            cx.notify();
        }))
        .into_any_element()
}

/// The fleet in one line, Fit, and the way to whatever needs you.
fn readout(
    galaxy: &Galaxy,
    agents: &[Agent],
    waiting: Vec<(String, Pt, Hsla)>,
    (width, height): (f32, f32),
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let working = agents.iter().filter(|agent| agent.info.working()).count();
    let line = format!(
        "{} machines · {} agents · {} working",
        galaxy.systems.len(),
        agents.len(),
        working
    );
    let (core, fit) = (galaxy.core, galaxy.fit(width, height));
    let pill = |id: &'static str, text: String| {
        div()
            .id(id)
            .debug_selector(move || id.into())
            .px(px(10.))
            .py(px(4.))
            .rounded_full()
            .cursor_pointer()
            .text_size(px(12.))
            .child(text)
    };
    div()
        .absolute()
        .top(px(10.))
        .left(px(10.))
        .right(px(10.))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .px(px(10.))
                .py(px(4.))
                .rounded_full()
                .bg(look.raised.opacity(0.85))
                .text_size(px(11.))
                .text_color(from_hex(&look.chrome.subtext))
                .child(line),
        )
        .child(div().flex_1())
        .child(
            pill("fleet-galaxy-fit", "Fit".into())
                .bg(look.raised.opacity(0.85))
                .text_color(look.ink)
                .on_click(cx.listener(move |desk, _, _, cx| {
                    desk.fleet_view
                        .playground
                        .camera
                        .look_at(core, fit, width, height);
                    cx.notify();
                })),
        )
        .when_some(waiting.first().map(|(_, _, c)| *c), |row, color| {
            let count = waiting.len();
            row.child(
                pill("fleet-galaxy-next-waiting", format!("{count} need you"))
                    .bg(color)
                    .text_color(from_hex(&look.chrome.theme.background))
                    .on_click(cx.listener(move |desk, _, _, cx| {
                        let view = &mut desk.fleet_view.playground;
                        // The waiting agent after the last one flown to, round again at the end.
                        let next = waiting
                            .iter()
                            .position(|(id, _, _)| Some(id) == view.last_waiting.as_ref())
                            .map_or(0, |at| (at + 1) % waiting.len());
                        let (id, foot, _) = &waiting[next];
                        view.last_waiting = Some(id.clone());
                        view.camera.look_at(*foot, 1., width, height);
                        cx.notify();
                    })),
            )
        })
        .into_any_element()
}

impl Paint {
    fn sky(
        &self,
        sky: &Sky,
        camera: &Camera,
        bounds: Bounds<Pixels>,
        time: f32,
        window: &mut gpui::Window,
    ) {
        let at = |world: Pt| -> Point<Pixels> {
            let s = camera.screen(world);
            point(bounds.origin.x + px(s.x), bounds.origin.y + px(s.y))
        };
        let scale = camera.scale;
        // Stars, each from its own seed, scattered over the visible scene.
        let (w, h) = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
        for index in 0..120 {
            let seed = plan::seed(&format!("star:{index}"));
            let twinkle = 0.2 + 0.3 * (1. + (time * (0.6 + seed.1) + seed.0 * 40.).sin()) / 2.;
            let star = point(
                bounds.origin.x + px(seed.0 * w),
                bounds.origin.y + px(seed.1 * h),
            );
            fill(
                window,
                &ellipse(star, 1. + seed.1, 1. + seed.1),
                self.faint.opacity(twinkle * 2.),
            );
        }
        let light = sky.light;
        if !sky.planets.is_empty() {
            dashed(
                window,
                at(sky.core),
                sky.orbit * scale,
                48,
                0.,
                self.faint,
                1.,
            );
        }
        for &(centre, rings, online) in &sky.planets {
            line(
                window,
                at(sky.core),
                at(centre),
                light.opacity(if online { 0.35 } else { 0.12 }),
                1.5,
            );
            for ring in 0..rings {
                let radius = (SHELL + RING_GAP * ring as f32) * scale;
                dashed(
                    window,
                    at(centre),
                    radius,
                    32,
                    0.,
                    self.faint.opacity(0.7),
                    1.,
                );
            }
        }
        self.arcs(&sky.links, sky.core, light, &at, time, window);
        for beam in &sky.beams {
            line(
                window,
                at(beam.from),
                at(beam.to),
                beam.color.opacity(0.45),
                1.,
            );
            if beam.waiting {
                // A beacon: a ring that swells and fades, once a second and a half.
                let swell = time.rem_euclid(1.5) / 1.5;
                let r = (30. + 26. * swell) * scale;
                ring(
                    window,
                    at(beam.to),
                    r,
                    beam.color.opacity(0.6 * (1. - swell)),
                    2.,
                );
            }
        }
        for &(centre, _, online) in &sky.planets {
            let edge = if online { light } else { self.faint };
            fill(
                window,
                &ellipse(at(centre), 20. * scale, 20. * scale),
                self.floor,
            );
            ring(window, at(centre), 20. * scale, edge, 2.);
            fill(window, &ellipse(at(centre), 6. * scale, 6. * scale), edge);
        }
        // The core: a pulsing heart in rotating rings.
        let core = at(sky.core);
        let pulse = 0.5 + 0.5 * (time * 2.).sin();
        fill(
            window,
            &ellipse(core, (30. + 4. * pulse) * scale, (30. + 4. * pulse) * scale),
            light.opacity(0.15),
        );
        fill(window, &ellipse(core, 14. * scale, 14. * scale), light);
        dashed(
            window,
            core,
            46. * scale,
            18,
            time * 0.25,
            light.opacity(0.7),
            2.,
        );
        dashed(
            window,
            core,
            60. * scale,
            48,
            -time * 0.12,
            light.opacity(0.4),
            4.,
        );
    }

    /// Links on the galaxy: arcs bowing away from the core, each with a pulse
    /// of light running along it and a short tail behind.
    fn arcs(
        &self,
        links: &[(Pt, Pt)],
        core: Pt,
        light: Hsla,
        at: &dyn Fn(Pt) -> Point<Pixels>,
        time: f32,
        window: &mut gpui::Window,
    ) {
        for (index, &(a, b)) in links.iter().enumerate() {
            let curve = (0..=24)
                .map(|step| at(galaxy::arc(a, b, core, step as f32 / 24.)))
                .collect::<Vec<_>>();
            polyline(window, &curve, light.opacity(0.3), 1.5);
            let head = (time * 0.35 + index as f32 * 0.37).rem_euclid(1.);
            let tail = (0..=6)
                .map(|step| {
                    at(galaxy::arc(
                        a,
                        b,
                        core,
                        (head - 0.12 + step as f32 * 0.02).clamp(0., 1.),
                    ))
                })
                .collect::<Vec<_>>();
            polyline(window, &tail, light.opacity(0.8), 2.5);
            let dot = at(galaxy::arc(a, b, core, head));
            fill(window, &ellipse(dot, 3.5, 3.5), light);
        }
    }

    /// Links on the matrix map: a faint wire between the two desks with
    /// three packets streaming along it.
    pub(super) fn packets(
        &self,
        links: &[(Pt, Pt)],
        camera: &Camera,
        bounds: Bounds<Pixels>,
        time: f32,
        window: &mut gpui::Window,
    ) {
        let at = |world: Pt| -> Point<Pixels> {
            let s = camera.screen(world);
            point(bounds.origin.x + px(s.x), bounds.origin.y + px(s.y))
        };
        let side = px(4. * camera.scale.max(0.6));
        for (index, &(a, b)) in links.iter().enumerate() {
            line(window, at(a), at(b), self.edge.opacity(0.25), 1.);
            for packet in 0..3 {
                let t = (time * 0.5 + packet as f32 / 3. + index as f32 * 0.21).rem_euclid(1.);
                let p = at(Pt {
                    x: a.x + (b.x - a.x) * t,
                    y: a.y + (b.y - a.y) * t,
                });
                let square = [
                    point(p.x - side / 2., p.y - side / 2.),
                    point(p.x + side / 2., p.y - side / 2.),
                    point(p.x + side / 2., p.y + side / 2.),
                    point(p.x - side / 2., p.y + side / 2.),
                ];
                fill(window, &square, self.edge.opacity(0.9));
            }
        }
    }
}

fn line(window: &mut gpui::Window, a: Point<Pixels>, b: Point<Pixels>, color: Hsla, width: f32) {
    polyline(window, &[a, b], color, width);
}

fn polyline(window: &mut gpui::Window, points: &[Point<Pixels>], color: Hsla, width: f32) {
    let mut path = gpui::PathBuilder::stroke(px(width));
    path.add_polygon(points, false);
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn ring(window: &mut gpui::Window, centre: Point<Pixels>, r: f32, color: Hsla, width: f32) {
    let mut path = gpui::PathBuilder::stroke(px(width));
    path.add_polygon(&ellipse(centre, r, r), true);
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// A circle as `dashes` dashes, each half its share of the way round, turned
/// by `turn` revolutions.
fn dashed(
    window: &mut gpui::Window,
    centre: Point<Pixels>,
    r: f32,
    dashes: usize,
    turn: f32,
    color: Hsla,
    width: f32,
) {
    let share = std::f32::consts::TAU / dashes as f32;
    for dash in 0..dashes {
        let start = turn * std::f32::consts::TAU + dash as f32 * share;
        let points = (0..=3)
            .map(|step| {
                let a = start + share * 0.5 * step as f32 / 3.;
                point(centre.x + px(a.cos() * r), centre.y + px(a.sin() * r))
            })
            .collect::<Vec<_>>();
        polyline(window, &points, color, width);
    }
}
