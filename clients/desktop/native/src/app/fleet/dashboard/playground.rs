//! The fleet's playground: the same agents as the cards, drawn as ghosts in
//! their host's room on an isometric campus under a camera the person drags,
//! scrolls and pinches. A click on a ghost opens that exact agent, as a card
//! does. One geometry (`plan`) places the painter's floors, the sprites, the
//! labels and the click targets, so they cannot drift apart.

mod plan;
mod scheme;
mod sprites;

pub(crate) use self::scheme::Scheme;

use std::time::{SystemTime, UNIX_EPOCH};

use gpui::prelude::*;
use gpui::{
    AnyElement, Bounds, ContentMask, Context, Corners, Hsla, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PathBuilder, PinchEvent, Pixels, Point, ScrollWheelEvent, Window,
    canvas, div, point, px, size,
};

use self::plan::{Plan, Pt, Strip, WALL_H};
use super::cards::{state_color, state_label, tile_id};
use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::fleet::{Agent, Reach};
use crate::app::skin::Look;

/// The sprite's drawn size at scale 1.
const GHOST: f32 = 64.;

/// The maps: skins over one geometry, the same two mobile ships.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Map {
    Office,
    Matrix,
}

impl Map {
    pub fn from_id(id: &str) -> Map {
        if id == "matrix" {
            Map::Matrix
        } else {
            Map::Office
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Map::Office => "office",
            Map::Matrix => "matrix",
        }
    }

    pub fn next(self) -> Map {
        match self {
            Map::Office => Map::Matrix,
            Map::Matrix => Map::Office,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Map::Office => "Office map",
            Map::Matrix => "Matrix map",
        }
    }
}
const SCALES: (f32, f32) = (0.45, 2.);

/// Where the person is looking: an offset in scene points and a scale.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Camera {
    pub offset: Pt,
    pub scale: f32,
    /// Whether the camera has been pointed at the first room that needs you.
    pub placed: bool,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            offset: Pt::default(),
            scale: 0.9,
            placed: false,
        }
    }
}

impl Camera {
    /// Centre `target` (world points) in a scene of `width` by `height`.
    fn look(&mut self, target: Pt, width: f32, height: f32) {
        self.offset = Pt {
            x: width / 2. - target.x * self.scale,
            y: height / 2. - target.y * self.scale,
        };
        self.placed = true;
    }

    /// Zoom by `delta` (0.1 is ten percent) about the scene's centre.
    fn pinch(&mut self, delta: f32, width: f32, height: f32) {
        let next = (self.scale * (1. + delta)).clamp(SCALES.0, SCALES.1);
        let ratio = next / self.scale;
        let centre = Pt {
            x: width / 2.,
            y: height / 2.,
        };
        self.offset = Pt {
            x: centre.x - (centre.x - self.offset.x) * ratio,
            y: centre.y - (centre.y - self.offset.y) * ratio,
        };
        self.scale = next;
    }

    fn screen(&self, world: Pt) -> Pt {
        Pt {
            x: self.offset.x + world.x * self.scale,
            y: self.offset.y + world.y * self.scale,
        }
    }
}

#[derive(Default)]
pub(crate) struct View {
    pub on: bool,
    pub camera: Camera,
    /// The last pointer position while the left button pans.
    pub drag: Option<Pt>,
    /// Where the camera first landed, which a fixture film moves from.
    #[cfg(feature = "ui-fixtures")]
    pub film_base: Option<Camera>,
}

#[cfg(feature = "ui-fixtures")]
impl View {
    /// A fixture film's camera: the placed camera panned by `pan` scene
    /// points, then zoomed by `zoom` about the centre of a `width` by
    /// `height` scene. Nothing moves until the scene has placed the camera.
    pub(crate) fn film_camera(&mut self, pan: (f32, f32), zoom: f32, width: f32, height: f32) {
        if !self.camera.placed {
            return;
        }
        let mut camera = *self.film_base.get_or_insert(self.camera);
        camera.offset.x += pan.0;
        camera.offset.y += pan.1;
        camera.pinch(zoom - 1., width, height);
        self.camera = camera;
    }
}

/// One ghost's spot and loop at this moment, ready to paint and to click.
struct Ghost {
    agent: Agent,
    foot: Pt,
    strip: Strip,
    depth: f32,
}

/// The campus under the camera, `width` by `height` in the panel.
pub(crate) fn scene(
    desk: &mut Desk,
    agents: &[Agent],
    width: f32,
    height: f32,
    time: f32,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    // Local, the registered machines, then any machine an agent names that
    // the registry does not (a fixture, or a host removed while attached).
    let mut hosts: Vec<Option<String>> = std::iter::once(None)
        .chain(desk.hosts.iter().map(|host| Some(host.name.clone())))
        .collect();
    for agent in agents {
        if !hosts.contains(&agent.destination.host) {
            hosts.push(agent.destination.host.clone());
        }
    }
    // A staged inventory (fixture captures) stands in for live machines.
    #[cfg(feature = "ui-fixtures")]
    let staged = desk.fleet_view.fixture.is_some();
    #[cfg(not(feature = "ui-fixtures"))]
    let staged = false;
    let hosts = hosts
        .into_iter()
        .map(|host| {
            let online = staged || matches!(desk.machine_reach(host.as_deref()), Reach::Connected);
            (host, online)
        })
        .collect();
    let plan = Plan::new(hosts, agents);
    if !desk.fleet_view.playground.camera.placed {
        desk.fleet_view
            .playground
            .camera
            .look(plan.focus(), width, height);
    }
    let camera = desk.fleet_view.playground.camera;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();
    let placed = &plan;
    let ghosts = plan
        .rooms
        .iter()
        .flat_map(|room| {
            let count = room.agents.len();
            room.agents.iter().enumerate().map(move |(index, agent)| {
                let id = tile_id(agent.destination.host.as_deref(), &agent.terminal);
                let tile = plan::spot(
                    (index, count),
                    plan::seed(&id),
                    time,
                    agent.info.working(),
                    room.field,
                );
                Ghost {
                    agent: agent.clone(),
                    foot: placed.place(tile),
                    strip: plan::strip(&agent.info, now_ms),
                    depth: tile.x + tile.y,
                }
            })
        })
        .collect::<Vec<_>>();

    let map = Map::from_id(&desk.prefs.playground_map);
    let scheme = Scheme::from_id(&desk.prefs.playground_scheme);
    let hex =
        |text: &str| u32::from_str_radix(text.trim_start_matches('#'), 16).unwrap_or(0xE7E2DA);
    let ramp = scheme.ramp(scheme::theme_ramp([
        hex(&look.chrome.muted),
        hex(&look.chrome.subtext),
        hex(&look.chrome.theme.foreground),
    ]));
    let accent = from_hex(&look.chrome.theme.accent);
    let background = from_hex(&look.chrome.theme.background);
    let paint = match map {
        Map::Office => Paint {
            map,
            ramp,
            floor: from_hex(&look.chrome.theme.surface),
            wall: look.raised,
            edge: look.border,
            faint: look.divider,
            shadow: background.opacity(0.5),
            lamp: accent.opacity(0.05),
        },
        Map::Matrix => Paint {
            map,
            ramp,
            floor: from_hex(&look.chrome.theme.surface),
            wall: background,
            edge: accent,
            faint: look.divider,
            shadow: background.opacity(0.6),
            lamp: accent.opacity(0.18),
        },
    };
    let labels = ghosts
        .iter()
        .map(|ghost| label(ghost, &camera, look))
        .chain(
            plan.rooms
                .iter()
                .map(|room| room_label(room, &plan, &camera, look)),
        )
        .collect::<Vec<_>>();
    let targets = ghosts
        .iter()
        .map(|ghost| target(ghost, &camera, cx))
        .collect::<Vec<_>>();
    let sprites = ghosts
        .iter()
        .map(|ghost| (ghost.foot, ghost.strip, ghost.depth))
        .collect::<Vec<_>>();

    div()
        .id("fleet-playground")
        .debug_selector(|| "fleet-playground".into())
        .relative()
        .w(px(width))
        .h(px(height))
        .overflow_hidden()
        .bg(from_hex(&look.chrome.theme.background))
        .cursor_grab()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|desk, event: &MouseDownEvent, _, cx| {
                desk.fleet_view.playground.drag = Some(pt(event.position));
                cx.notify();
            }),
        )
        .on_mouse_move(cx.listener(|desk, event: &MouseMoveEvent, _, cx| {
            let view = &mut desk.fleet_view.playground;
            if event.pressed_button != Some(MouseButton::Left) {
                return;
            }
            if let Some(last) = view.drag {
                let now = pt(event.position);
                view.camera.offset.x += now.x - last.x;
                view.camera.offset.y += now.y - last.y;
                view.drag = Some(now);
                cx.notify();
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|desk, _: &MouseUpEvent, _, cx| {
                desk.fleet_view.playground.drag = None;
                cx.notify();
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|desk, _: &MouseUpEvent, _, cx| {
                desk.fleet_view.playground.drag = None;
                cx.notify();
            }),
        )
        .on_scroll_wheel(cx.listener(|desk, event: &ScrollWheelEvent, _, cx| {
            let delta = event.delta.pixel_delta(px(20.));
            let camera = &mut desk.fleet_view.playground.camera;
            camera.offset.x += delta.x.as_f32();
            camera.offset.y += delta.y.as_f32();
            cx.stop_propagation();
            cx.notify();
        }))
        .on_pinch(cx.listener(move |desk, event: &PinchEvent, _, cx| {
            desk.fleet_view
                .playground
                .camera
                .pinch(event.delta, width, height);
            cx.notify();
        }))
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    window.with_content_mask(Some(ContentMask { bounds }), |window| {
                        paint.campus(&plan, &camera, bounds, time, window);
                        paint.ghosts(&sprites, &camera, bounds, time, window);
                    });
                },
            )
            .absolute()
            .size_full(),
        )
        .children(labels)
        .children(targets)
        .into_any_element()
}

fn pt(position: Point<Pixels>) -> Pt {
    Pt {
        x: position.x.as_f32(),
        y: position.y.as_f32(),
    }
}

/// The ghost's name and state word under its feet; the colours the cards use.
fn label(ghost: &Ghost, camera: &Camera, look: &Look) -> AnyElement {
    let at = camera.screen(ghost.foot);
    let color = state_color(&ghost.agent.info, look);
    div()
        .absolute()
        .left(px(at.x - 70.))
        .top(px(at.y + 4.))
        .w(px(140.))
        .flex()
        .flex_col()
        .items_center()
        .text_size(px(11.))
        .child(
            div()
                .truncate()
                .text_color(look.ink)
                .child(ghost.agent.info.name.clone()),
        )
        .child(
            div()
                .text_size(px(10.))
                .text_color(color)
                .child(state_label(&ghost.agent.info)),
        )
        .into_any_element()
}

fn room_label(room: &plan::Room, plan: &Plan, camera: &Camera, look: &Look) -> AnyElement {
    let at = camera.screen(plan.place(Pt {
        x: room.tiles.x,
        y: room.tiles.y,
    }));
    let name = room.host.clone().unwrap_or_else(|| "Local".into());
    div()
        .absolute()
        .left(px(at.x + 8.))
        .top(px(at.y - WALL_H * camera.scale - 20.))
        .flex()
        .gap(px(6.))
        .text_size(px(12.))
        .text_color(look.ink)
        .child(name)
        .when(!room.online, |label| {
            label.child(
                div()
                    .text_color(from_hex(&look.chrome.muted))
                    .child("offline"),
            )
        })
        .into_any_element()
}

/// The clickable ghost: the card's own selector, so the tests and the keyboard
/// path treat it as the same tile.
fn target(ghost: &Ghost, camera: &Camera, cx: &mut Context<Desk>) -> AnyElement {
    let at = camera.screen(ghost.foot);
    let side = GHOST * camera.scale;
    let selector = tile_id(
        ghost.agent.destination.host.as_deref(),
        &ghost.agent.terminal,
    );
    let agent = ghost.agent.clone();
    div()
        .id(selector.clone())
        .debug_selector(move || selector.clone())
        .absolute()
        .left(px(at.x - side / 2.))
        .top(px(at.y - side * 0.95))
        .w(px(side))
        .h(px(side))
        .cursor_pointer()
        .on_click(
            cx.listener(move |desk, _, window, cx| {
                desk.open_fleet_agent(agent.clone(), window, cx)
            }),
        )
        .into_any_element()
}

/// The painter's colours, taken from the theme once per frame.
#[derive(Clone, Copy)]
struct Paint {
    map: Map,
    ramp: scheme::Ramp,
    floor: Hsla,
    wall: Hsla,
    edge: Hsla,
    faint: Hsla,
    shadow: Hsla,
    lamp: Hsla,
}

impl Paint {
    fn campus(
        &self,
        plan: &Plan,
        camera: &Camera,
        bounds: Bounds<Pixels>,
        time: f32,
        window: &mut Window,
    ) {
        let at = |world: Pt| -> Point<Pixels> {
            let s = camera.screen(world);
            point(bounds.origin.x + px(s.x), bounds.origin.y + px(s.y))
        };
        let wall_h = px(WALL_H * camera.scale);
        let mut rooms = plan.rooms.iter().collect::<Vec<_>>();
        rooms.sort_by(|a, b| (a.tiles.x + a.tiles.y).total_cmp(&(b.tiles.x + b.tiles.y)));
        for room in rooms {
            let t = room.tiles;
            let corner = |across: f32, down: f32| at(plan.place(Pt { x: across, y: down }));
            let floor = [
                corner(t.x, t.y),
                corner(t.max_x(), t.y),
                corner(t.max_x(), t.max_y()),
                corner(t.x, t.max_y()),
            ];
            fill(
                window,
                &floor,
                if room.online {
                    self.floor
                } else {
                    self.faint.opacity(0.3)
                },
            );
            if self.map == Map::Office && room.online && !room.agents.is_empty() {
                let lamp = at(plan.place(t.mid()));
                let halo = ellipse(lamp, 90. * camera.scale, 45. * camera.scale);
                fill(window, &halo, self.lamp);
            }
            let edge = if room.online { self.edge } else { self.faint };
            if self.map == Map::Matrix {
                // The floor's own outline, and a terminal on every desk.
                stroke(
                    window,
                    &floor,
                    edge.opacity(if room.online { 0.5 } else { 0.25 }),
                    1.,
                );
                self.terminals(room, plan, &at, camera.scale, time, window);
            }
            let faces = [
                (corner(t.x, t.max_y()), corner(t.x, t.y)),
                (corner(t.x, t.y), corner(room.door.0, t.y)),
                (corner(room.door.1, t.y), corner(t.max_x(), t.y)),
            ];
            for (a, b) in faces {
                let face = [a, b, point(b.x, b.y - wall_h), point(a.x, a.y - wall_h)];
                fill(
                    window,
                    &face,
                    self.wall.opacity(if room.online { 0.9 } else { 0.4 }),
                );
                if self.map == Map::Matrix && room.online {
                    // A glow: a wide faint pass under the thin bright edge.
                    stroke(window, &face, edge.opacity(0.22), 6.);
                }
                stroke(window, &face, edge, 1.5);
            }
        }
        if self.map == Map::Matrix {
            // One scanline sweeping the scene; still with the clock.
            let height = bounds.size.height.as_f32().max(1.);
            let y = bounds.origin.y + px((time * 50.).rem_euclid(height));
            let right = bounds.origin.x + bounds.size.width;
            let line = [
                point(bounds.origin.x, y),
                point(right, y),
                point(right, y + px(2.)),
                point(bounds.origin.x, y + px(2.)),
            ];
            fill(window, &line, self.lamp);
        }
    }

    /// A terminal on every desk with a blinking cursor: the matrix map's desk.
    fn terminals(
        &self,
        room: &plan::Room,
        plan: &Plan,
        at: &dyn Fn(Pt) -> Point<Pixels>,
        scale: f32,
        time: f32,
        window: &mut Window,
    ) {
        let blink = time.rem_euclid(1.) < 0.5;
        let count = room.agents.len();
        for (index, agent) in room.agents.iter().enumerate() {
            let id = tile_id(room.host.as_deref(), &agent.terminal);
            let tile = plan::spot((index, count), plan::seed(&id), 0., false, room.field);
            let foot = at(plan.place(tile));
            let (w, h) = (px(16. * scale), px(12. * scale));
            let origin = point(foot.x - w / 2., foot.y - px(14. * scale));
            let screen = [
                origin,
                point(origin.x + w, origin.y),
                point(origin.x + w, origin.y + h),
                point(origin.x, origin.y + h),
            ];
            stroke(window, &screen, self.edge, 1.);
            if blink {
                let (l, r) = (origin.x + px(3. * scale), origin.x + px(7. * scale));
                let (top, bottom) = (origin.y + px(3. * scale), origin.y + px(9. * scale));
                fill(
                    window,
                    &[
                        point(l, top),
                        point(r, top),
                        point(r, bottom),
                        point(l, bottom),
                    ],
                    self.edge,
                );
            }
        }
    }

    fn ghosts(
        &self,
        sprites: &[(Pt, Strip, f32)],
        camera: &Camera,
        bounds: Bounds<Pixels>,
        time: f32,
        window: &mut Window,
    ) {
        let mut order = sprites.iter().collect::<Vec<_>>();
        order.sort_by(|a, b| a.2.total_cmp(&b.2));
        let side = GHOST * camera.scale;
        for (foot, strip, _) in order {
            let s = camera.screen(*foot);
            let centre = point(bounds.origin.x + px(s.x), bounds.origin.y + px(s.y));
            let shadow = ellipse(centre, 22. * camera.scale, 7. * camera.scale);
            fill(window, &shadow, self.shadow);
            let Some(sheet) = sprites::sheet(self.ramp) else {
                continue;
            };
            let (image, frame) = sheet.frame(*strip, time);
            let origin = point(centre.x - px(side / 2.), centre.y - px(side * 0.95));
            let cell = Bounds {
                origin,
                size: size(px(side), px(side)),
            };
            let _ = window.paint_image(cell, cell, Corners::default(), image, frame, false);
        }
    }
}

fn ellipse(centre: Point<Pixels>, rx: f32, ry: f32) -> Vec<Point<Pixels>> {
    (0..20)
        .map(|i| {
            let a = i as f32 / 20. * std::f32::consts::TAU;
            point(centre.x + px(a.cos() * rx), centre.y + px(a.sin() * ry))
        })
        .collect()
}

fn fill(window: &mut Window, points: &[Point<Pixels>], color: Hsla) {
    let mut path = PathBuilder::fill();
    path.add_polygon(points, true);
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn stroke(window: &mut Window, points: &[Point<Pixels>], color: Hsla, width: f32) {
    let mut path = PathBuilder::stroke(px(width));
    path.add_polygon(points, true);
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}
