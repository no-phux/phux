//! Native design-review captures using the shipping composition. Never dials a host.

use super::super::{Agent, Destination};
use crate::app::desk::{Desk, Modal};
use crate::connection::{AgentAttention, AgentInfo, AgentState};
use gpui::{App, AppContext as _, px, size};
use std::{path::PathBuf, time::Duration};

fn inventory() -> Vec<Agent> {
    [
        (
            "Claude",
            "claude",
            "Reviewing the authentication boundary",
            None,
            "phux / API",
            AgentState::Working,
        ),
        (
            "Pi",
            "pi",
            "Ship the new fleet experience",
            None,
            "phux / Desktop",
            AgentState::Working,
        ),
        (
            "Codex",
            "codex",
            "Approval needed before running migration",
            Some("studio"),
            "platform / Database",
            AgentState::Blocked,
        ),
        (
            "OpenCode",
            "opencode",
            "Keyboard navigation · regression suite",
            Some("studio"),
            "phux / Tests",
            AgentState::Done,
        ),
        (
            "Gemini",
            "gemini",
            "Explore the next release",
            Some("build-linux"),
            "research",
            AgentState::Idle,
        ),
        (
            "Grok",
            "grok",
            "Waiting for the next instruction",
            Some("build-linux"),
            "docs / Guides",
            AgentState::Idle,
        ),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (name, kind, title, host, session, state))| Agent {
        destination: Destination {
            host: host.map(str::to_owned),
            authority: None,
            key: None,
            name: session.into(),
        },
        terminal: phux_protocol::ResourceId::local(index as u32 + 1),
        boot: Some("fixture".into()),
        info: AgentInfo {
            name: name.into(),
            kind: Some(kind.into()),
            state,
            attention: AgentAttention::None,
            changed_at_ms: 1,
        },
        title: title.into(),
    })
    .collect()
}

pub(crate) fn capture(
    root: gpui::WindowHandle<gpui_kit::component::Root>,
    directory: PathBuf,
    cx: &mut App,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&directory)?;
    cx.spawn(async move |cx| {
        for (name, width, height, light, empty, playground, map) in [
            ("fleet-dark", 1280., 900., false, false, false, "office"),
            ("fleet-light", 1280., 900., true, false, false, "office"),
            ("fleet-narrow", 520., 780., false, false, false, "office"),
            ("fleet-empty", 900., 700., false, true, false, "office"),
            (
                "fleet-playground",
                1280.,
                900.,
                false,
                false,
                true,
                "office",
            ),
            (
                "fleet-playground-light",
                1280.,
                900.,
                true,
                false,
                true,
                "office",
            ),
            (
                "fleet-playground-matrix",
                1280.,
                900.,
                false,
                false,
                true,
                "matrix",
            ),
            (
                "fleet-playground-mint",
                1280.,
                900.,
                false,
                false,
                true,
                "office",
            ),
        ] {
            root.update(cx, |root, window, cx| {
                let desk = root.view().clone().downcast::<Desk>().expect("desk");
                window.resize(size(px(width), px(height)));
                desk.update(cx, |desk, cx| {
                    let theme = crate::settings::theme::themes()
                        .iter()
                        .find(|theme| (theme.appearance == "light") == light)
                        .expect("theme")
                        .id
                        .clone();
                    desk.update_prefs(
                        |prefs| {
                            prefs.agent_fleet = true;
                            prefs.animations = false;
                            prefs.theme_id = theme;
                            prefs.playground_map = map.to_owned();
                            prefs.playground_scheme = if name.ends_with("mint") {
                                "mint"
                            } else {
                                "theme"
                            }
                            .to_owned();
                        },
                        cx,
                    );
                    desk.fleet_view.open();
                    desk.fleet_view.playground.on = playground;
                    desk.fleet_view.fixture = Some(if empty { Vec::new() } else { inventory() });
                    desk.modal = Modal::AgentFleet;
                    cx.notify();
                });
                window.refresh();
            })
            .expect("fixture window");
            cx.background_executor()
                .timer(Duration::from_millis(800))
                .await;
            cx.update_window(*root, |_, window, cx| window.draw(cx).clear(cx))
                .expect("draw fleet");
            root.update(cx, |_, window, _| {
                window
                    .render_to_image()
                    .expect("fleet scene capture")
                    .save(directory.join(format!("{name}.png")))
                    .expect("write fleet PNG");
            })
            .expect("capture fleet");
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
    Ok(())
}

/// One beat of the film: from `at` seconds, these settings hold.
struct Beat {
    at: f32,
    theme: &'static str,
    playground: bool,
    map: &'static str,
    scheme: &'static str,
}

const fn beat(
    at: f32,
    theme: &'static str,
    playground: bool,
    map: &'static str,
    scheme: &'static str,
) -> Beat {
    Beat {
        at,
        theme,
        playground,
        map,
        scheme,
    }
}

/// The scripted fleet film: the cards, the playground's two maps, the piece's
/// colour schemes, a run of themes, then the cards again in a light theme.
const BEATS: &[Beat] = &[
    beat(0., "midnight", false, "office", "theme"),
    beat(3.5, "midnight", true, "office", "theme"),
    beat(9., "midnight", true, "matrix", "theme"),
    beat(14., "midnight", true, "matrix", "mint"),
    beat(15.2, "midnight", true, "matrix", "aurora"),
    beat(16.4, "midnight", true, "matrix", "ember"),
    beat(17.6, "midnight", true, "matrix", "theme"),
    beat(18.8, "nord", true, "office", "theme"),
    beat(20., "dracula", true, "office", "theme"),
    beat(21.2, "mocha", true, "office", "theme"),
    beat(22.4, "tokyo-night", true, "office", "theme"),
    beat(23.6, "latte", true, "office", "theme"),
    beat(26., "latte", false, "office", "theme"),
];
const FILM_SECONDS: f32 = 29.;
/// The playground scene's size in the film's 1440 by 900 window.
const SCENE: (f32, f32) = (1152., 570.);

/// The camera's slow push: from the whole campus, centred, to a little
/// closer, eased over the playground's stretch of the film.
fn glide(time: f32) -> ((f32, f32), f32) {
    let ease = |x: f32| x * x * (3. - 2. * x);
    let p = ease(((time - 3.5) / 22.5).clamp(0., 1.));
    let lerp = |a: f32, b: f32| a + (b - a) * p;
    (
        (lerp(PAN.0, PAN_END.0), lerp(PAN.1, PAN_END.1)),
        lerp(ZOOM.0, ZOOM.1),
    )
}
const PAN: (f32, f32) = (250., 85.);
const PAN_END: (f32, f32) = (250., 70.);
const ZOOM: (f32, f32) = (1.0, 1.2);
const FILM_FPS: f32 = 30.;

/// Render the film frame by frame into `directory` as `frame-00000.png`.
/// The panel's clock is set to each frame's time before it draws, so the
/// motion is exact however long a frame takes to render.
pub(crate) fn film(
    root: gpui::WindowHandle<gpui_kit::component::Root>,
    directory: PathBuf,
    cx: &mut App,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&directory)?;
    cx.spawn(async move |cx| {
        let frames = (FILM_SECONDS * FILM_FPS) as usize;
        // Warm every map and theme once so no frame waits on an asset.
        for (index, current) in BEATS.iter().enumerate().chain(BEATS.iter().enumerate()) {
            stage(&root, current, index as f32, true, cx);
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
        }
        let mut shown = usize::MAX;
        for frame in 0..frames {
            let time = frame as f32 / FILM_FPS;
            let index = BEATS.iter().rposition(|b| b.at <= time).unwrap_or(0);
            stage(&root, &BEATS[index], time, index != shown, cx);
            shown = index;
            cx.update_window(*root, |_, window, cx| window.draw(cx).clear(cx))
                .expect("draw film frame");
            root.update(cx, |_, window, _| {
                window
                    .render_to_image()
                    .expect("film frame capture")
                    .save(directory.join(format!("frame-{frame:05}.png")))
                    .expect("write film frame");
            })
            .expect("capture film frame");
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
    Ok(())
}

/// Hold `current`'s settings and put the panel's clock at `time`; `apply`
/// writes the settings, which only a new beat needs.
fn stage(
    root: &gpui::WindowHandle<gpui_kit::component::Root>,
    current: &Beat,
    time: f32,
    apply: bool,
    cx: &mut gpui::AsyncApp,
) {
    root.update(cx, |root, window, cx| {
        let desk = root.view().clone().downcast::<Desk>().expect("desk");
        if window.viewport_size() != size(px(1440.), px(900.)) {
            window.resize(size(px(1440.), px(900.)));
        }
        desk.update(cx, |desk, cx| {
            if desk.fleet_view.fixture.is_none() {
                desk.fleet_view.open();
                desk.fleet_view.fixture = Some(inventory());
                desk.modal = Modal::AgentFleet;
            }
            if apply {
                desk.update_prefs(
                    |prefs| {
                        prefs.agent_fleet = true;
                        prefs.animations = true;
                        prefs.theme_id = current.theme.to_owned();
                        prefs.playground_map = current.map.to_owned();
                        prefs.playground_scheme = current.scheme.to_owned();
                    },
                    cx,
                );
                desk.fleet_view.playground.on = current.playground;
            }
            desk.fleet_view.opened =
                Some(std::time::Instant::now() - Duration::from_secs_f32(time));
            let (pan, zoom) = glide(time);
            desk.fleet_view
                .playground
                .film_camera(pan, zoom, SCENE.0, SCENE.1);
            cx.notify();
        });
        window.refresh();
    })
    .expect("film stage");
}
