//! Every playground view over a populated fleet, painted through Rune's
//! real renderer in both house modes and increased contrast, with an agent
//! that needs you picked so the whole HUD shows. `RUNE_PLAYGROUND_SNAP=DIR`
//! writes each as `DIR/NAME.png`; without it the frames are still built and
//! painted, so a view that stops drawing fails here.

use phux_client_runtime::control::{Event, PaneDescriptor, ResourceDescriptor, SessionDescriptor};
use phux_protocol::ids::{ResourceId, ResourceKind, SessionKey};
use rune::{Env, Runtime};

use crate::desk::Desk;

/// Pixels per point: a Retina window.
const SCALE: u8 = 2;
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

fn session(id: u32, name: &str) -> SessionDescriptor {
    SessionDescriptor {
        id,
        name: name.to_owned(),
        attached_client_count: 1,
        keep_empty: true,
        created_at_unix_secs: 0,
        key: SessionKey::new([u8::try_from(id).unwrap_or(0); 16]),
        touched: Some(u64::from(100 - id)),
        lifetime: None,
        revision: None,
        members: 0,
        arrangement: None,
        awaiting_organization: false,
        project: None,
        project_link: None,
    }
}

fn pane(id: u32, session: &SessionDescriptor, title: &str) -> PaneDescriptor {
    PaneDescriptor {
        terminal_id: ResourceId::local(id),
        session_id: session.id,
        session_name: session.name.clone(),
        cols: 120,
        rows: 40,
        title: Some(title.to_owned()),
        cwd: Some("~/workspace/phux".to_owned()),
        is_focused: id == 1,
        key: None,
        revision: None,
        name: Some(title.to_owned()),
        activation: None,
        exec: None,
        foreground: None,
    }
}

fn agent(id: u32, kind: &str, state: &str) -> Event {
    Event::AgentMetadata {
        terminal_id: ResourceId::local(id),
        value: Some(
            format!(r#"{{"name":"{kind}","kind":"{kind}","state":"{state}"}}"#).into_bytes(),
        ),
    }
}

/// A window on a server with agents in every state, `open` showing.
fn window(dark: bool, open: fn(&mut Desk)) -> Runtime<Desk> {
    let replica = crate::test_support::terminal::Replica::new(120, 40, 1);
    let mut desk = crate::test_support::window();
    desk.connection.client = Some(std::sync::Arc::clone(&replica.client));
    let (phux, site) = (session(1, "phux"), session(2, "website"));
    let panes = vec![
        pane(1, &phux, "restyle sidebar"),
        pane(2, &phux, "cargo test"),
        pane(3, &phux, "review #2617"),
        pane(4, &site, "docs build"),
        pane(5, &site, "android build"),
    ];
    let mut topology = crate::desk::topology(vec![phux, site], panes);
    topology.resources = (topology.panes.iter())
        .map(|p| ResourceDescriptor {
            id: p.terminal_id.clone(),
            kind: ResourceKind::Terminal,
            session_id: p.session_id,
            parent: None,
            title: p.title.clone(),
            cells: Some((120, 40)),
        })
        .collect();
    let mut rt = Runtime::new(
        desk,
        crate::ui::view,
        SIZE,
        Env::new(dark, f32::from(SCALE)),
    );
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

fn settle(rt: &mut Runtime<Desk>) {
    for t in [0.0, 0.5, 1.0, 2.0, 3.0] {
        rt.frame(t);
    }
}

fn paint(gpu: &rune::render::Gpu, rt: &mut Runtime<Desk>, name: &str) {
    rt.set_srgb_blend(true);
    settle(rt);
    let scale = f32::from(SCALE);
    let (w, h) = ((SIZE.0 * scale) as u32, (SIZE.1 * scale) as u32);
    let pixels = crate::ui::offscreen_as_app(gpu, rt, (w, h));
    assert!(
        pixels.as_chunks::<4>().0.iter().any(|p| p[0] != p[1]),
        "{name} painted nothing but grey"
    );
    if let Some(dir) = std::env::var_os("RUNE_PLAYGROUND_SNAP") {
        let dir = std::path::Path::new(&dir);
        std::fs::create_dir_all(dir).unwrap();
        rune::render::write_png(&dir.join(format!("{name}.png")), w, h, &pixels).unwrap();
    }
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
