//! The galaxy map: the fleet as a command center under the playground's
//! camera. Agents are glowing nodes on rings round their machines; the HUD
//! counts what is working, [ FIT ] shows everything again, and the need-you
//! button flies to and selects each waiting agent in turn. A planet click
//! flies into that machine's system; hovering a node puts the reticle on it.

use gpui::prelude::*;
use gpui::{AnyElement, ContentMask, Context, FontWeight, Hsla, canvas, div, px};

use super::super::cards::{state_color, state_label, tile_id};
use super::galaxy::{self, Detail, Galaxy, Mood};
use super::nodes::{Node, Planet, Sky};
use super::plan::Pt;
use super::{Camera, Paint, stage};
use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::fleet::Agent;
use crate::app::skin::Look;

/// A node's hit target, never under 44 points.
const HIT: f32 = 44.;

/// One placed agent: who, where, and its tile id.
struct Placed {
    agent: Agent,
    at: Pt,
    id: String,
    color: Hsla,
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
    let mut placed = Vec::new();
    let mut nodes = Vec::new();
    for system in &galaxy.systems {
        for (index, agent) in system.agents.iter().enumerate() {
            let at = system.point(index, time, agent.info.working());
            let color = state_color(&agent.info, look);
            nodes.push(Node {
                planet: system.centre,
                at,
                color,
                mood: Mood::of(&agent.info),
            });
            placed.push(Placed {
                agent: agent.clone(),
                at,
                id: tile_id(agent.destination.host.as_deref(), &agent.terminal),
                color,
            });
        }
    }
    // The reticle: the hovered node, else the keyboard's selected agent.
    let hovered = view.hovered.clone();
    let selected = desk.fleet_view.selected.as_ref();
    let focus = placed
        .iter()
        .find(|p| Some(&p.id) == hovered.as_ref())
        .or_else(|| {
            placed
                .iter()
                .find(|p| selected.is_some_and(|s| p.agent.same_target(s)))
        })
        .map(|p| p.at);
    let waiting = placed
        .iter()
        .filter(|p| p.agent.info.needs_you())
        .map(|p| (p.agent.clone(), p.id.clone(), p.at))
        .collect::<Vec<_>>();
    let needs = placed
        .iter()
        .find(|p| Mood::of(&p.agent.info) == Mood::Waiting)
        .map(|p| p.color);
    let related = placed.iter().map(|p| p.agent.clone()).collect::<Vec<_>>();
    let sky = Sky {
        core: galaxy.core,
        orbit: galaxy.orbit,
        rim: galaxy.rim,
        planets: galaxy
            .systems
            .iter()
            .map(|s| Planet {
                centre: s.centre,
                rings: s.rings(),
                online: s.online,
            })
            .collect(),
        links: galaxy::relations(&related)
            .into_iter()
            .map(|(a, b)| (placed[a].at, placed[b].at))
            .collect(),
        light: paint.edge,
        ping: needs,
        focus,
        soft: look.light,
        nodes,
    };
    let detail = Detail::at(camera.scale);
    let labels = placed
        .iter()
        .filter(|p| detail != Detail::Waiting || p.agent.info.needs_you())
        .map(|p| node_label(p, &camera, detail, look))
        .collect::<Vec<_>>();
    let targets = placed
        .iter()
        .map(|p| node_target(p, &camera, cx))
        .collect::<Vec<_>>();
    let planets = galaxy
        .systems
        .iter()
        .flat_map(|system| planet(system, galaxy.core, &camera, (width, height), look, cx))
        .collect::<Vec<_>>();
    let ticker = ticker(agents, look);
    let readout = readout(&galaxy, agents, waiting, (width, height), look, cx);
    stage(width, height, look, cx)
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    window.with_content_mask(Some(ContentMask { bounds }), |window| {
                        paint.sky(&sky, &camera, bounds, time, window);
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
        .children(ticker)
        .into_any_element()
}

/// Letters spaced with hair spaces: the HUD's tracked capitals.
fn tracked(text: &str) -> String {
    let mut out = String::new();
    for (i, c) in text.to_uppercase().chars().enumerate() {
        if i > 0 {
            out.push('\u{200A}');
        }
        out.push(c);
    }
    out
}

fn short(name: &str) -> String {
    if name.chars().count() > 14 {
        name.chars().take(13).chain(std::iter::once('…')).collect()
    } else {
        name.to_owned()
    }
}

/// The agent's name, and its state word in tracked capitals in its colour.
fn node_label(p: &Placed, camera: &Camera, detail: Detail, look: &Look) -> AnyElement {
    let at = camera.screen(p.at);
    let gap = 18. * camera.scale.max(0.6);
    div()
        .absolute()
        .left(px(at.x - 70.))
        .top(px(at.y + gap))
        .w(px(140.))
        .flex()
        .flex_col()
        .items_center()
        .font_family("monospace")
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(look.ink)
                .child(short(&p.agent.info.name)),
        )
        .when(detail == Detail::Full, |label| {
            label.child(
                div()
                    .text_size(px(9.5))
                    .text_color(p.color)
                    .child(tracked(&state_label(&p.agent.info))),
            )
        })
        .into_any_element()
}

/// The clickable node: the card's own selector, so the tests and the
/// keyboard path treat it as the same tile. Hover puts the reticle on it.
fn node_target(p: &Placed, camera: &Camera, cx: &mut Context<Desk>) -> AnyElement {
    let at = camera.screen(p.at);
    let side = (28. * camera.scale).max(HIT);
    let selector = p.id.clone();
    let id = p.id.clone();
    let agent = p.agent.clone();
    div()
        .id(selector.clone())
        .debug_selector(move || selector.clone())
        .absolute()
        .left(px(at.x - side / 2.))
        .top(px(at.y - side / 2.))
        .w(px(side))
        .h(px(side))
        .cursor_pointer()
        .on_hover(cx.listener(move |desk, hovered: &bool, _, cx| {
            let slot = &mut desk.fleet_view.playground.hovered;
            if *hovered {
                *slot = Some(id.clone());
            } else if slot.as_deref() == Some(id.as_str()) {
                *slot = None;
            }
            cx.notify();
        }))
        .on_click(
            cx.listener(move |desk, _, window, cx| {
                desk.open_fleet_agent(agent.clone(), window, cx)
            }),
        )
        .into_any_element()
}

/// A machine's name under its planet in tracked capitals, and how many
/// agents it carries, on the planet's outer side where no agent sits; a
/// click on the planet flies into its system.
fn planet(
    system: &galaxy::System,
    core: Pt,
    camera: &Camera,
    (width, height): (f32, f32),
    look: &Look,
    cx: &mut Context<Desk>,
) -> [AnyElement; 2] {
    let at = camera.screen(system.centre);
    let name = system.host.clone().unwrap_or_else(|| "Local".into());
    let selector = format!("fleet-galaxy-planet-{name}");
    let centre = system.centre;
    let count = system.agents.len();
    let note = if !system.online {
        "offline".to_owned()
    } else if count == 1 {
        "1 agent".to_owned()
    } else {
        format!("{count} agents")
    };
    let (dx, dy) = (system.centre.x - core.x, system.centre.y - core.y);
    let len = (dx * dx + dy * dy).sqrt().max(1.);
    let (ux, uy) = (dx / len, dy / len);
    let reach = 26. * camera.scale + 12.;
    let (lx, ly) = (
        at.x + ux * (reach + 50. * ux.abs()),
        at.y + uy * (reach + 14.),
    );
    let label = div()
        .absolute()
        .left(px(lx - 70.))
        .top(px(ly - 14.))
        .w(px(140.))
        .flex()
        .flex_col()
        .items_center()
        .font_family("monospace")
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(look.ink)
                .child(tracked(&short(&name))),
        )
        .child(
            div()
                .text_size(px(9.))
                .text_color(from_hex(&look.chrome.muted))
                .child(tracked(&note)),
        )
        .into_any_element();
    let target = div()
        .id(selector.clone())
        .debug_selector(move || selector.clone())
        .absolute()
        .left(px(at.x - HIT / 2.))
        .top(px(at.y - HIT / 2.))
        .size(px(HIT))
        .cursor_pointer()
        .on_click(cx.listener(move |desk, _, _, cx| {
            desk.fleet_view
                .playground
                .camera
                .look_at(centre, 1., width, height);
            cx.notify();
        }))
        .into_any_element();
    [label, target]
}

/// The HUD: the fleet in one line under its eyebrow, [ FIT ], and the way to
/// whatever needs you.
fn readout(
    galaxy: &Galaxy,
    agents: &[Agent],
    waiting: Vec<(Agent, String, Pt)>,
    (width, height): (f32, f32),
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let working = agents.iter().filter(|agent| agent.info.working()).count();
    let line = format!(
        "{} machines  ·  {} agents  ·  {} working",
        galaxy.systems.len(),
        agents.len(),
        working
    );
    let accent = from_hex(&look.chrome.theme.accent);
    let needs = waiting
        .first()
        .map(|(agent, _, _)| state_color(&agent.info, look));
    let (core, fit) = (galaxy.core, galaxy.fit(width, height));
    let button = |id: &'static str, text: String, color: Hsla| {
        div()
            .id(id)
            .debug_selector(move || id.into())
            .px(px(8.))
            .py(px(4.))
            .rounded(px(3.))
            .cursor_pointer()
            .font_family("monospace")
            .text_size(px(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(color)
            .bg(color.opacity(0.1))
            .hover(|style| style.bg(color.opacity(0.2)))
            .child(text)
    };
    div()
        .absolute()
        .top(px(16.))
        .left(px(24.))
        .right(px(24.))
        .flex()
        .items_start()
        .gap(px(8.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.))
                .font_family("monospace")
                .child(
                    div()
                        .text_size(px(10.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(accent)
                        .child(tracked("phux // fleet")),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(from_hex(&look.chrome.subtext))
                        .child(tracked(&line)),
                ),
        )
        .child(div().flex_1())
        .child(
            button("fleet-galaxy-fit", "[ FIT ]".into(), look.ink).on_click(cx.listener(
                move |desk, _, _, cx| {
                    desk.fleet_view
                        .playground
                        .camera
                        .look_at(core, fit, width, height);
                    cx.notify();
                },
            )),
        )
        .when_some(needs, |row, color| {
            let count = waiting.len();
            row.child(
                button(
                    "fleet-galaxy-next-waiting",
                    format!("[ {count} NEED YOU ]"),
                    color,
                )
                .on_click(cx.listener(move |desk, _, _, cx| {
                    let view = &mut desk.fleet_view.playground;
                    // The waiting agent after the last one flown to, round again at the end.
                    let next = waiting
                        .iter()
                        .position(|(_, id, _)| Some(id) == view.last_waiting.as_ref())
                        .map_or(0, |at| (at + 1) % waiting.len());
                    let (agent, id, at) = &waiting[next];
                    view.last_waiting = Some(id.clone());
                    view.camera.look_at(*at, 1., width, height);
                    desk.fleet_view.selected = Some(agent.clone());
                    cx.notify();
                })),
            )
        })
        .into_any_element()
}

/// The latest change in the fleet, in one line at the bottom left.
fn ticker(agents: &[Agent], look: &Look) -> Option<AnyElement> {
    let latest = agents
        .iter()
        .filter(|agent| agent.info.changed_at_ms > 0)
        .max_by_key(|agent| agent.info.changed_at_ms)?;
    let text = format!(
        "{} on {}  →  {}",
        latest.info.name,
        latest.destination.host_label(),
        state_label(&latest.info)
    );
    Some(
        div()
            .absolute()
            .left(px(24.))
            .bottom(px(16.))
            .font_family("monospace")
            .text_size(px(10.))
            .text_color(from_hex(&look.chrome.muted))
            .child(tracked(&text))
            .into_any_element(),
    )
}
