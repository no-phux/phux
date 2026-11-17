//! The chrome's gallery: the window's main surfaces with a populated
//! sidebar, painted through Rune's real renderer in dark and light, on the
//! desktop and the phone. `RUNE_GALLERY_SNAP=DIR` writes each as
//! `DIR/NAME.png`; without it the frames are still built and painted, so a
//! surface that stops drawing fails here.

use phux_client_runtime::control::Event;
use phux_client_runtime::control::{PaneDescriptor, SessionDescriptor};
use phux_protocol::ids::{ResourceId, SessionKey};
use rune::{Env, Runtime};

use crate::desk::Desk;

const SCALE: f32 = 2.0;

fn session(id: u32, name: &str, key: u8) -> SessionDescriptor {
    SessionDescriptor {
        id,
        name: name.to_owned(),
        attached_client_count: 1,
        keep_empty: true,
        created_at_unix_secs: 0,
        key: SessionKey::new([key; 16]),
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

fn pane(id: u32, session: &SessionDescriptor, title: &str, cwd: &str) -> PaneDescriptor {
    PaneDescriptor {
        terminal_id: ResourceId::local(id),
        session_id: session.id,
        session_name: session.name.clone(),
        cols: 120,
        rows: 40,
        title: Some(title.to_owned()),
        cwd: Some(cwd.to_owned()),
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

const SCREEN: &str = "\x1b[2J\x1b[H\x1b[1;38;5;208m✻\x1b[0m Welcome to \x1b[1mClaude Code\x1b[0m\r\n\r\n\
\x1b[2m> \x1b[0mrestyle the sidebar on rune-kit\r\n\r\n\
\x1b[38;5;245m⏺\x1b[0m Reading \x1b[1mclients/rune/src/ui/sidebar.rs\x1b[0m\r\n\
\x1b[38;5;245m⏺\x1b[0m Running \x1b[1mcargo test --locked\x1b[0m\r\n\
   Compiling phux-rune v0.1.0\r\n\
    Finished `test` profile [unoptimized + debuginfo] target(s) in 41.2s\r\n\
     Running unittests src/lib.rs\r\n\
test ui::gallery_tests::the_gallery_paints ... \x1b[32mok\x1b[0m\r\n\
test result: \x1b[32mok\x1b[0m. 1412 passed; 0 failed; 3 ignored\r\n\r\n\
\x1b[38;5;214m?\x1b[0m Commit these changes? \x1b[2m(y/n)\x1b[0m ";

/// A window attached to a populated server: three sessions, agents in every
/// state, the first terminal on screen with real output.
fn window(size: (f32, f32), dark: bool, theme: Option<&str>) -> Runtime<Desk> {
    let mut replica = crate::test_support::terminal::Replica::new(120, 40, 1);
    replica.feed(SCREEN.to_owned());
    let mut desk = crate::test_support::window();
    desk.settings.theme = theme.and_then(crate::theme::find);
    desk.connection.client = Some(std::sync::Arc::clone(&replica.client));
    desk.connection.layout = Some(phux_client_core::layout::LayoutNode::Leaf(
        replica.terminal.clone(),
    ));
    desk.connection.shown = Some(crate::desk::Shown {
        resource: replica.terminal.clone(),
        view: Some(replica.views[0]),
    });
    let (phux, site, mobile) = (
        session(1, "phux", 1),
        session(2, "website", 2),
        session(3, "phux-mobile", 3),
    );
    let panes = vec![
        pane(1, &phux, "restyle sidebar", "~/workspace/phux"),
        pane(2, &phux, "cargo test", "~/workspace/phux"),
        pane(3, &phux, "review #2617", "~/workspace/phux"),
        pane(4, &site, "docs build", "~/workspace/website"),
        pane(5, &site, "zsh", "~/workspace/website"),
        pane(6, &mobile, "ios snapshots", "~/workspace/phux-mobile"),
    ];
    let mut topology = crate::desk::topology(vec![phux, site, mobile], panes);
    topology.resources = (topology.panes.iter())
        .map(|p| phux_client_runtime::control::ResourceDescriptor {
            id: p.terminal_id.clone(),
            kind: phux_protocol::ids::ResourceKind::Terminal,
            session_id: p.session_id,
            parent: None,
            title: p.title.clone(),
            cells: Some((120, 40)),
        })
        .collect();
    let mut rt = Runtime::new(desk, crate::ui::view, size, Env::new(dark, SCALE));
    rt.on_key = Some(Desk::key);
    // The embedded client reports its own one-terminal graph on the first
    // frame; the fixture's replaces it after.
    rt.frame(0.0);
    rt.update(move |d, _| {
        d.connection.topology = Some(topology);
        for event in [
            agent(1, "claude", "working"),
            agent(3, "codex", "blocked"),
            agent(4, "claude", "done"),
            agent(6, "opencode", "working"),
        ] {
            d.activities.apply(&event, &[]);
        }
    });
    rt
}

fn settle(rt: &mut Runtime<Desk>) {
    for t in [0.0, 0.5, 1.0, 2.0, 3.0] {
        rt.frame(t);
    }
}

fn paint(gpu: &rune::render::Gpu, rt: &mut Runtime<Desk>, size: (f32, f32), name: &str) {
    settle(rt);
    let (w, h) = ((size.0 * SCALE) as u32, (size.1 * SCALE) as u32);
    let mut renderer = rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
    let pixels = crate::ui::offscreen(gpu, &mut renderer, rt, (w, h));
    assert!(
        pixels.as_chunks::<4>().0.iter().any(|p| p[0] != p[1]),
        "{name} painted nothing but grey"
    );
    if let Some(dir) = std::env::var_os("RUNE_GALLERY_SNAP") {
        let dir = std::path::Path::new(&dir);
        std::fs::create_dir_all(dir).unwrap();
        rune::render::write_png(&dir.join(format!("{name}.png")), w, h, &pixels).unwrap();
    }
}

#[test]
fn the_gallery_paints() {
    const DESKTOP: (f32, f32) = (1280.0, 800.0);
    let gpu = rune::render::Gpu::headless();
    for (dark, theme, tag) in [
        (true, None, "dark"),
        (false, None, "light"),
        (true, Some("catppuccin-mocha"), "mocha"),
    ] {
        let mut rt = window(DESKTOP, dark, theme);
        settle(&mut rt);
        let rows = (rt.elements().into_iter())
            .filter(|e| e.key.as_deref().is_some_and(|k| k.starts_with("pane-")))
            .count();
        assert!(rows >= 6, "the sidebar lists every terminal: {rows}");
        paint(&gpu, &mut rt, DESKTOP, &format!("window-{tag}"));
        rt.update(|d, _| d.toggle_palette());
        paint(&gpu, &mut rt, DESKTOP, &format!("palette-{tag}"));
        rt.update(|d, _| {
            d.close_palette();
            d.settings_open = true;
        });
        paint(&gpu, &mut rt, DESKTOP, &format!("settings-{tag}"));
        rt.update(|d, _| {
            d.settings_open = false;
            d.toggle_inbox();
        });
        paint(&gpu, &mut rt, DESKTOP, &format!("inbox-{tag}"));
    }
}
