//! Every playground view over a populated fleet, painted through Rune's
//! real renderer in both house modes and increased contrast, with an agent
//! that needs you picked so the whole HUD shows. `RUNE_PLAYGROUND_SNAP=DIR`
//! writes each as `DIR/NAME.png`; without it the frames are still built and
//! painted, so a view that stops drawing fails here.

use rune::{Env, Runtime};

use crate::desk::Desk;
use crate::test_support::server::{self, agent, pane, session, settle};

/// Pixels per point: a Retina window.
const SCALE: f32 = 2.0;
const SIZE: (f32, f32) = (1280.0, 800.0);

/// A view: its key prefix, how it opens and how it picks the next agent
/// that needs you.
type Scene = (&'static str, fn(&mut Desk), fn(&mut Desk));

const SCENES: [Scene; 6] = [
    ("galaxy", Desk::toggle_galaxy, Desk::next_waiting),
    ("reef", Desk::toggle_reef, Desk::reef_next_waiting),
    ("pitch", Desk::toggle_pitch, Desk::pitch_next_waiting),
    ("kitchen", Desk::toggle_kitchen, Desk::kitchen_next_waiting),
    ("pitlane", Desk::toggle_pitlane, Desk::pitlane_next_waiting),
    ("hive", Desk::toggle_hive, Desk::hive_next_waiting),
];

/// A window on a server with agents in every state, `open` showing.
fn window(dark: bool, open: fn(&mut Desk)) -> Runtime<Desk> {
    let replica = crate::test_support::terminal::Replica::new(120, 40, 1);
    let mut desk = crate::test_support::window();
    desk.connection.client = Some(std::sync::Arc::clone(&replica.client));
    let (phux, site) = (session(1, "phux", 1), session(2, "website", 2));
    let panes = vec![
        pane(1, &phux, "restyle sidebar", None),
        pane(2, &phux, "cargo test", None),
        pane(3, &phux, "review #2617", None),
        pane(4, &site, "docs build", None),
        pane(5, &site, "android build", None),
    ];
    let mut topology = crate::desk::topology(vec![phux, site], panes);
    server::terminal_resources(&mut topology);
    let mut rt = Runtime::new(desk, crate::ui::view, SIZE, Env::new(dark, SCALE));
    rt.on_key = Some(Desk::key);
    rt.frame(0.0);
    rt.update(move |d, _| {
        d.connection.topology = Some(topology);
        for event in [
            agent(1, "claude", "working"),
            agent(2, "codex", "blocked"),
            agent(3, "claude", "done"),
            agent(4, "opencode", "working"),
            agent(5, "gemini", "idle"),
        ] {
            d.activities.apply(&event, &[]);
        }
        open(d);
    });
    rt
}

fn paint(gpu: &rune::render::Gpu, rt: &mut Runtime<Desk>, name: &str) {
    server::paint(gpu, rt, SIZE, SCALE, "RUNE_PLAYGROUND_SNAP", name);
}

fn has(rt: &Runtime<Desk>, key: &str) -> bool {
    rt.elements().iter().any(|e| e.key.as_deref() == Some(key))
}

#[test]
fn every_playground_paints_its_hud() {
    let gpu = rune::render::Gpu::headless();
    for (scene, open, next) in SCENES {
        for (dark, more, tag) in [
            (true, false, "dark"),
            (false, false, "light"),
            (true, true, "contrast-more-dark"),
            (false, true, "contrast-more-light"),
        ] {
            let mut rt = window(dark, open);
            if more {
                rt.set_system_increased_contrast(true);
            }
            settle(&mut rt);
            assert!(has(&rt, "playground-esc"), "{scene}: the way out");
            assert!(has(&rt, &format!("{scene}-close")), "{scene}: close");
            assert!(
                has(&rt, &format!("{scene}-next-waiting")),
                "{scene}: one agent needs you"
            );
            rt.update(move |d, _| next(d));
            paint(&gpu, &mut rt, &format!("{scene}-{tag}"));
        }
    }
}
