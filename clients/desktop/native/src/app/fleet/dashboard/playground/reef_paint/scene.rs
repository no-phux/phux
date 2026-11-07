//! The reef as an element: steps the simulation, then the canvas, the
//! name pills, the click targets and the readout.

use super::*;

pub(in super::super) fn scene(
    desk: &mut Desk,
    hosts: Vec<(Option<String>, bool)>,
    agents: &[Agent],
    (width, height): (f32, f32),
    time: f32,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let hosts = hosts.into_iter().map(|(host, _)| host).collect::<Vec<_>>();
    let night = luminance(&look.chrome.theme.background) < 0.5;
    let slow = cx.reduce_motion();
    let view = &mut desk.fleet_view.playground;
    let focus_id = view.last_waiting.clone();
    let sim = view
        .reef
        .get_or_insert_with(|| Box::new(Sim::new(width, height, &hosts, agents, night)));
    sim.sync(width, height, &hosts, agents, night);
    // The panel clock restarts when the fleet reopens; that is no step back.
    let elapsed = if time > sim.clock {
        (time - sim.clock).min(0.25)
    } else {
        0.
    };
    sim.clock = time;
    if elapsed == 0. {
        sim.settle_light();
    }
    let dt = if slow { elapsed * 0.4 } else { elapsed };
    let mut left = dt;
    while left > 0. {
        let step = left.min(1. / 30.);
        sim.step(step);
        left -= step;
    }
    let focus = focus_id
        .as_ref()
        .and_then(|id| sim.fish.iter().position(|f| &f.id == id));
    sim.camera(focus, dt, slow || dt == 0.);
    let frame = (**sim).clone();

    let mut labels = Vec::new();
    for (index, zone) in frame.zones.iter().enumerate() {
        let (count, waiting) = frame
            .fish
            .iter()
            .filter(|f| f.zone == index)
            .fold((0, 0), |(n, w), f| {
                (n + 1, w + usize::from(f.mood == Mood::Waiting))
            });
        let at = screen(&frame, zone.cx, frame.wave_y(zone.cx) - 36. * frame.s);
        labels.push(pill(
            at,
            format!("{}  {count}", zone.name),
            false,
            (waiting > 0).then(|| waiting.to_string()),
        ));
    }
    for (index, f) in frame.fish.iter().enumerate() {
        let l = f.len(frame.s);
        let shown = Some(index) == focus || frame.hovered.as_ref() == Some(&f.id);
        if f.mood == Mood::Waiting {
            let at = screen(&frame, f.x, f.y - l * 0.9);
            labels.push(pill(
                at,
                format!("{} · needs you", f.agent.info.name),
                true,
                None,
            ));
        } else if shown {
            let at = screen(&frame, f.x, f.y + l * 0.95);
            labels.push(pill(at, f.agent.info.name.clone(), false, None));
        }
    }
    let targets = frame
        .fish
        .iter()
        .map(|f| target(f, &frame, cx))
        .collect::<Vec<_>>();
    let readout = readout(&frame, focus_id.is_some(), look, cx);
    let paint_frame = frame;
    stage(width, height, look, cx)
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    window.with_content_mask(Some(ContentMask { bounds }), |window| {
                        draw(&paint_frame, focus, bounds, window);
                    });
                },
            )
            .absolute()
            .size_full(),
        )
        .children(labels)
        .children(targets)
        .child(readout)
        .into_any_element()
}

/// Relative luminance of a `#rrggbb` theme colour, 0 to 1.
pub(super) fn luminance(hex: &str) -> f32 {
    let v = u32::from_str_radix(hex.trim_start_matches('#').get(..6).unwrap_or("000000"), 16)
        .unwrap_or(0);
    let c = |shift: u32| ((v >> shift) & 0xFF) as f32 / 255.;
    0.2126 * c(16) + 0.7152 * c(8) + 0.0722 * c(0)
}

/// A scene point on the panel, under the reef camera.
pub(super) fn screen(sim: &Sim, x: f32, y: f32) -> P {
    let m = view(sim, 1., 0., 0.);
    (m[0] * x + m[4], m[3] * y + m[5])
}

/// The camera as an affine map, `p` of the way in (far layers move less).
pub(super) fn view(sim: &Sim, p: f32, ox: f32, oy: f32) -> [f32; 6] {
    let z = 1. + (sim.cam.z - 1.) * p;
    let cx = sim.w / 2. + (sim.cam.x - sim.w / 2.) * p;
    let cy = sim.h / 2. + (sim.cam.y - sim.h / 2.) * p;
    [
        z,
        0.,
        0.,
        z,
        ox + sim.w / 2. - cx * z,
        oy + sim.h / 2. - cy * z,
    ]
}

/// A name pill centred on `at`; amber for a fish that needs you, with an
/// optional amber count badge (the buoys' waiting count).
pub(super) fn pill(at: P, text: String, amber: bool, badge: Option<String>) -> AnyElement {
    let (bg, ink) = if amber {
        (rgba(AMBER, 0.95), rgba([42., 22., 0.], 1.))
    } else {
        (rgba([4., 20., 34.], 0.72), rgba([228., 245., 247.], 1.))
    };
    div()
        .absolute()
        .left(px(at.0 - 160.))
        .top(px(at.1 - 9.5))
        .w(px(320.))
        .flex()
        .justify_center()
        .child(
            div()
                .h(px(19.))
                .px(px(7.))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded_full()
                .bg(bg)
                .text_size(px(11.))
                .text_color(ink)
                .child(text)
                .when_some(badge, |row, badge| {
                    row.child(
                        div()
                            .size(px(14.))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(rgba(AMBER, 1.))
                            .text_color(rgba([42., 22., 0.], 1.))
                            .text_size(px(10.))
                            .child(badge),
                    )
                }),
        )
        .into_any_element()
}

/// The fish's click target: the card's own selector, so tests and the
/// keyboard path treat it as that agent's tile. Hover shows its name.
pub(super) fn target(f: &Fish, sim: &Sim, cx: &mut Context<Desk>) -> AnyElement {
    let at = screen(sim, f.x, f.y);
    let side = (f.len(sim.s) * 1.5 + 12.) * sim.cam.z;
    let selector = f.id.clone();
    let agent = f.agent.clone();
    let id = f.id.clone();
    div()
        .id(selector.clone())
        .debug_selector(move || selector.clone())
        .absolute()
        .left(px(at.0 - side / 2.))
        .top(px(at.1 - side / 2.))
        .w(px(side))
        .h(px(side))
        .cursor_pointer()
        .on_hover(cx.listener(move |desk, over: &bool, _, cx| {
            if let Some(sim) = desk.fleet_view.playground.reef.as_mut() {
                if *over {
                    sim.hovered = Some(id.clone());
                } else if sim.hovered.as_ref() == Some(&id) {
                    sim.hovered = None;
                }
                cx.notify();
            }
        }))
        .on_click(
            cx.listener(move |desk, _, window, cx| {
                desk.open_fleet_agent(agent.clone(), window, cx)
            }),
        )
        .into_any_element()
}

/// The fleet in one line, the way back to the whole reef, and the way to
/// whatever needs you.
pub(super) fn readout(sim: &Sim, focused: bool, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let working = sim.fish.iter().filter(|f| f.mood == Mood::Working).count();
    let waiting = sim
        .fish
        .iter()
        .filter(|f| f.mood == Mood::Waiting)
        .map(|f| f.id.clone())
        .collect::<Vec<_>>();
    let line = format!(
        "{} reefs · {} agents · {} working",
        sim.zones.len(),
        sim.fish.len(),
        working
    );
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
    let count = waiting.len();
    div()
        .absolute()
        .bottom(px(10.))
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
                .bg(rgba([4., 20., 34.], 0.62))
                .text_size(px(11.))
                .text_color(rgba([228., 245., 247.], 1.))
                .child(line),
        )
        .child(div().flex_1())
        .when(focused, |row| {
            row.child(
                pill("fleet-reef-fit", "Whole reef".into())
                    .bg(look.raised.opacity(0.85))
                    .text_color(look.ink)
                    .on_click(cx.listener(|desk, _, _, cx| {
                        desk.fleet_view.playground.last_waiting = None;
                        cx.notify();
                    })),
            )
        })
        .when(count > 0, |row| {
            row.child(
                pill(
                    "fleet-reef-next-waiting",
                    format!("{count} need{} you", if count == 1 { "s" } else { "" }),
                )
                .bg(rgba(AMBER, 1.))
                .text_color(rgba([42., 22., 0.], 1.))
                .on_click(cx.listener(move |desk, _, _, cx| {
                    let view = &mut desk.fleet_view.playground;
                    let next = waiting
                        .iter()
                        .position(|id| Some(id) == view.last_waiting.as_ref())
                        .map_or(0, |at| (at + 1) % waiting.len());
                    view.last_waiting = Some(waiting[next].clone());
                    cx.notify();
                })),
            )
        })
        .into_any_element()
}
