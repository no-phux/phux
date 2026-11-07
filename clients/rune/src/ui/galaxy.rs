//! The galaxy command center: the fleet as a core inside a ticked bezel with
//! a radar sweep, each machine a wireframe planet on one orbit, and the
//! connected machine's agents as state nodes on rings round it. Beams carry
//! pulses outward; related agents (one session, or one project) are joined by
//! a bowed beam. Drag pans, the wheel pans (zooms with ⌘ or Ctrl), a pinch
//! zooms, a double click or [ FIT ] fits it all again. A press on an agent
//! shows its terminal; one on a planet flies to it.

mod sky;

use rune::{Color, Cx, El, LineCap, Shape, TextStyle, canvas, col, el, row, text};

use self::sky::{Sky, State};
use super::sidebar::mark_color;
use super::{Ink, TITLEBAR_H, overlay, pressable};
use crate::activity::Mark;
use crate::desk::Desk;
use crate::galaxy::{self, Detail, Kin, Pt};

const NAME: TextStyle = TextStyle::new(11.0, 500.0).mono();
const WORD: TextStyle = TextStyle::new(9.5, 600.0).mono().tracking(1.5);
const HOST: TextStyle = TextStyle::new(11.0, 650.0).mono().tracking(1.5);
const META: TextStyle = TextStyle::new(9.0, 500.0).mono().tracking(1.2);
const EYEBROW: TextStyle = TextStyle::new(10.0, 650.0).mono().tracking(2.0);
const STATS: TextStyle = TextStyle::new(11.0, 500.0).mono().tracking(1.0);
const BUTTON: TextStyle = TextStyle::new(11.0, 650.0).mono().tracking(1.5);

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
    let needs = p.marks.alert;
    let light = if waiting > 0 { needs } else { p.accent };
    let ground_light = crate::theme::on_fill(p.bg) != Color::WHITE;

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
                project: star.project.as_deref(),
            }
        })
        .collect();

    let mut sky = Sky {
        camera,
        shape: Shape::new(),
        light: ground_light,
    };
    let rim = plan.bezel();
    sky = sky.dust(plan.core, rim, time, p.dim).bezel(
        plan.core,
        rim,
        time,
        light,
        (waiting > 0).then_some(needs),
    );
    sky = sky.orbits(&plan, p.separator);
    for (planet, system) in planets.iter().zip(&plan.systems) {
        sky = sky.beam(plan.core, system.centre, time, light, planet.online);
    }
    for (a, b) in galaxy::related(&kin) {
        sky = sky.kin(placed[a].2, placed[b].2, plan.core, time, p.accent);
    }
    let mut picked_at = None;
    for &(at, i, point) in &placed {
        let star = &planets[at].stars[i];
        let color = mark_color(star.mark, p);
        sky = sky.beam(
            plan.systems[at].centre,
            point,
            time,
            color,
            planets[at].online,
        );
        if desk.galaxy.picked.as_ref() == Some(&star.terminal) {
            picked_at = Some(point);
        }
    }
    for (planet, system) in planets.iter().zip(&plan.systems) {
        let color = if planet.online { light } else { p.dim };
        sky = sky.planet(system.centre, time, color, planet.online);
    }
    for &(at, i, point) in &placed {
        let star = &planets[at].stars[i];
        sky = sky.node(
            point,
            State::of(star.mark),
            mark_color(star.mark, p),
            needs,
            time,
        );
    }
    if let Some(point) = picked_at {
        sky = sky.reticle(plan.core, point, time, p.text);
    }
    sky = sky.core(plan.core, time, light);
    let mut shape = sky.shape;
    let [w, h] = desk.galaxy.viewport;
    shape = corners(shape, w, h, light.alpha(0.5));

    let detail = Detail::at(camera.scale);
    let mut labels: Vec<El> = Vec::new();
    for (planet, system) in planets.iter().zip(&plan.systems) {
        let centre = system.centre;
        let at = camera.screen(centre);
        // Only the connected machine lists its agents; another says what its
        // session listing says, or nothing.
        let note = if !planet.stars.is_empty() {
            Some(format!("{} agents", planet.stars.len()))
        } else if !planet.online {
            Some("offline".to_owned())
        } else {
            planet.note.clone()
        };
        labels.push(
            pressable(format!("galaxy-planet-{}", planet.name), p)
                .absolute(at[0] - 70.0, at[1] - 22.0)
                .w(140.0)
                .radius(8.0)
                .pt(52.0)
                .pb(4.0)
                .label(planet.name.clone())
                .on_press(move |desk: &mut Desk| desk.fly_to(centre))
                .child(
                    col()
                        .w_full()
                        .items_center()
                        .child(text(planet.name.to_uppercase()).font(HOST).color(p.text))
                        .children(note.map(|n| text(n.to_uppercase()).font(META).color(p.dim))),
                ),
        );
    }
    for &(at, i, point) in &placed {
        let star = &planets[at].stars[i];
        let s = camera.screen(point);
        let (terminal, session) = (star.terminal.clone(), star.session.clone());
        let word = word(star.mark);
        let named = match detail {
            Detail::Urgent => star.mark.needs_you(),
            _ => true,
        };
        let worded = detail == Detail::Full || (named && detail == Detail::Urgent);
        labels.push(
            pressable(format!("galaxy-star-{:?}", star.terminal), p)
                .absolute(s[0] - 60.0, s[1] - 22.0)
                .w(120.0)
                .radius(8.0)
                .pt(40.0)
                .label(format!("{}, {word}", star.name))
                .on_press(move |desk: &mut Desk| desk.open_star(terminal.clone(), &session))
                .children(named.then(|| {
                    col()
                        .w_full()
                        .items_center()
                        .child(text(truncate(&star.name, 14)).font(NAME).color(p.text))
                        .children(worded.then(|| {
                            text(word.to_uppercase())
                                .font(WORD)
                                .color(mark_color(star.mark, p))
                        }))
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
    let block = col()
        .absolute(30.0, TITLEBAR_H + 16.0)
        .gap(4.0)
        .child(text("PHUX // FLEET").font(EYEBROW).color(light))
        .child(
            text(format!(
                "{hosts} MACHINES  ·  {agents} AGENTS  ·  {working} WORKING"
            ))
            .font(STATS)
            .color(p.dim),
        );
    let actions = row()
        .absolute((w - 336.0).max(0.0), TITLEBAR_H + 12.0)
        .w(306.0)
        .gap(6.0)
        .items_center()
        .justify_end()
        .child(
            pressable("galaxy-fit", p)
                .px(8.0)
                .py(5.0)
                .radius(6.0)
                .label("Fit the whole galaxy")
                .on_press(Desk::fit_galaxy)
                .child(text("[ FIT ]").font(BUTTON).color(p.text)),
        )
        .children((waiting > 0).then(|| {
            pressable("galaxy-next-waiting", p)
                .px(8.0)
                .py(5.0)
                .radius(6.0)
                .label("Show the next agent that needs you")
                .on_press(Desk::next_waiting)
                .child(
                    text(format!("[ {waiting} NEED YOU ]"))
                        .font(BUTTON)
                        .color(needs),
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
            canvas(shape)
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
        .child(block)
        .child(actions)
}

/// Brackets in the view's four corners, in screen points.
fn corners(mut shape: Shape, w: f32, h: f32, color: Color) -> Shape {
    let (inset, leg) = (10.0, 18.0);
    let top = TITLEBAR_H.min(h);
    for (x, y, dx, dy) in [
        (inset, top + inset, 1.0, 1.0),
        (w - inset, top + inset, -1.0, 1.0),
        (inset, h - inset, 1.0, -1.0),
        (w - inset, h - inset, -1.0, -1.0),
    ] {
        let points = vec![[x, y + dy * leg], [x, y], [x + dx * leg, y]];
        shape = shape.stroke_with(points, false, 1.5, LineCap::Butt, color);
    }
    shape
}

fn truncate(name: &str, max: usize) -> String {
    if name.chars().count() <= max {
        return name.to_owned();
    }
    let mut cut: String = name.chars().take(max - 1).collect();
    cut.push('…');
    cut
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
