//! The chrome's gallery (§5.2): the window's main surfaces with a populated
//! sidebar, painted through Rune's real renderer, on the desktop and the
//! phone, over the design fixture: six themes, the accessibility modes,
//! three locales and a page of forced states. `RUNE_GALLERY_SNAP=DIR` writes
//! each as `DIR/NAME.png`; without it the frames are still built and
//! painted, so a surface that stops drawing fails here.

use phux_client_runtime::control::Event;
use phux_client_runtime::control::{PaneDescriptor, SessionDescriptor};
use phux_protocol::ids::{ResourceId, SessionKey};
use rune::{Env, Runtime};

use crate::desk::Desk;
use crate::theme::Theme;

mod fixture;
mod states;

use fixture::{Locale, Look};

const SCALE: f32 = 2.0;
const DESKTOP: (f32, f32) = (1280.0, 800.0);
const PHONE: (f32, f32) = (393.0, 852.0);

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

/// A window attached to a populated server: three sessions, agents in
/// every state the wire carries, the first terminal on screen with real
/// output, named in `locale`.
fn window(
    size: (f32, f32),
    dark: bool,
    theme: Option<&'static Theme>,
    locale: Locale,
) -> Runtime<Desk> {
    window_on(size, dark, theme, locale, false)
}

fn window_on(
    size: (f32, f32),
    dark: bool,
    theme: Option<&'static Theme>,
    locale: Locale,
    phone: bool,
) -> Runtime<Desk> {
    let mut replica = crate::test_support::terminal::Replica::new(120, 40, 1);
    replica.feed(SCREEN.to_owned());
    let mut desk = crate::test_support::window().with_phone(phone);
    desk.settings.theme = theme;
    desk.connection.client = Some(std::sync::Arc::clone(&replica.client));
    desk.connection.layout = Some(phux_client_core::layout::LayoutNode::Leaf(
        replica.terminal.clone(),
    ));
    desk.connection.shown = Some(crate::desk::Shown {
        resource: replica.terminal.clone(),
        view: Some(replica.views[0]),
    });
    let t = |s: &str| locale.say(s);
    let (phux, site, mobile) = (
        session(1, &t("phux"), 1),
        session(2, &t("website"), 2),
        session(3, &t("phux-mobile"), 3),
    );
    let panes = vec![
        pane(1, &phux, &t("restyle sidebar"), "~/workspace/phux"),
        pane(2, &phux, &t("cargo test"), "~/workspace/phux"),
        pane(3, &phux, &t("review #2617"), "~/workspace/phux"),
        pane(4, &site, &t("docs build"), "~/workspace/website"),
        pane(5, &site, "zsh", "~/workspace/website"),
        pane(6, &mobile, &t("ios snapshots"), "~/workspace/phux-mobile"),
        pane(7, &mobile, &t("android build"), "~/workspace/phux-mobile"),
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
            agent(7, "gemini", "idle"),
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

/// The frame as the app shows it: blended in sRGB, as every window is.
fn paint(gpu: &rune::render::Gpu, rt: &mut Runtime<Desk>, size: (f32, f32), name: &str) {
    rt.set_srgb_blend(true);
    settle(rt);
    let (w, h) = ((size.0 * SCALE) as u32, (size.1 * SCALE) as u32);
    let pixels = crate::ui::offscreen_as_app(gpu, rt, (w, h));
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

/// A surface page: its name and how the window opens it.
type Page = (&'static str, fn(&mut Desk));

/// The window, then its palette, settings and inbox over it. Each page
/// paints from a fresh runtime: a new renderer per frame would otherwise
/// miss the glyphs an earlier frame already uploaded to its atlas.
fn surfaces(gpu: &rune::render::Gpu, make: impl Fn() -> Runtime<Desk>, tag: &str) {
    let mut rt = make();
    settle(&mut rt);
    let rows = (rt.elements().into_iter())
        .filter(|e| e.key.as_deref().is_some_and(|k| k.starts_with("pane-")))
        .count();
    assert!(rows >= 6, "{tag}: the sidebar lists every terminal: {rows}");
    paint(gpu, &mut rt, DESKTOP, &format!("window-{tag}"));
    let pages: [Page; 3] = [
        ("palette", Desk::toggle_palette),
        ("settings", |d| d.settings_open = true),
        ("inbox", Desk::toggle_inbox),
    ];
    for (page, open) in pages {
        let mut rt = make();
        settle(&mut rt);
        rt.update(|d, _| open(d));
        paint(gpu, &mut rt, DESKTOP, &format!("{page}-{tag}"));
    }
}

#[test]
fn the_gallery_paints() {
    let gpu = rune::render::Gpu::headless();
    for look in Look::ALL {
        let make = || window(DESKTOP, look.dark(), look.theme(), Locale::En);
        surfaces(&gpu, make, look.tag());
    }
    // Rune's one built-in Catppuccin, kept so the before-and-after set
    // still lines up.
    let mocha = crate::theme::find("catppuccin-mocha");
    surfaces(&gpu, || window(DESKTOP, true, mocha, Locale::En), "mocha");
}

#[test]
fn the_gallery_paints_the_phone() {
    let gpu = rune::render::Gpu::headless();
    for look in [Look::HouseDark, Look::HouseLight] {
        let mut rt = window_on(PHONE, look.dark(), look.theme(), Locale::En, true);
        paint(&gpu, &mut rt, PHONE, &format!("phone-{}", look.tag()));
    }
}

#[test]
fn the_gallery_paints_every_mode() {
    let gpu = rune::render::Gpu::headless();
    for look in [Look::HouseDark, Look::HouseLight] {
        let tag = look.tag();
        let fresh = || window(DESKTOP, look.dark(), look.theme(), Locale::En);

        let mut rt = fresh();
        rt.set_system_increased_contrast(true);
        paint(&gpu, &mut rt, DESKTOP, &format!("mode-contrast-more-{tag}"));
        rt.update(|d, _| d.toggle_palette());
        paint(
            &gpu,
            &mut rt,
            DESKTOP,
            &format!("mode-contrast-more-palette-{tag}"),
        );

        let mut rt = fresh();
        rt.set_system_contrast(Some(fixture::forced_colors(look.dark())));
        paint(&gpu, &mut rt, DESKTOP, &format!("mode-forced-colors-{tag}"));

        let mut rt = fresh();
        rt.update(|d, _| {
            d.settings.glass = 100;
            d.set_reduce_transparency(true);
        });
        paint(
            &gpu,
            &mut rt,
            DESKTOP,
            &format!("mode-reduce-transparency-{tag}"),
        );

        let mut rt = fresh();
        rt.set_system_reduced_motion(true);
        paint(&gpu, &mut rt, DESKTOP, &format!("mode-reduce-motion-{tag}"));

        let mut rt = fresh();
        rt.update(|d, _| {
            d.set_text_scale(fixture::LARGEST_TEXT);
        });
        paint(&gpu, &mut rt, DESKTOP, &format!("mode-largest-text-{tag}"));
    }
}

#[test]
fn the_gallery_paints_every_locale() {
    let gpu = rune::render::Gpu::headless();
    for locale in [Locale::PseudoLong, Locale::Arabic] {
        for look in [Look::HouseDark, Look::HouseLight] {
            let mut rt = window(DESKTOP, look.dark(), look.theme(), locale);
            paint(
                &gpu,
                &mut rt,
                DESKTOP,
                &format!("locale-{}-{}", locale.tag(), look.tag()),
            );
        }
    }
}

#[test]
fn the_gallery_paints_the_states_page() {
    let gpu = rune::render::Gpu::headless();
    for look in Look::ALL {
        for (more, suffix) in [(false, ""), (true, "-contrast-more")] {
            let mut rt = states::page(look.dark(), look.theme());
            if more {
                rt.set_system_increased_contrast(true);
            }
            paint(
                &gpu,
                &mut rt,
                states::SIZE,
                &format!("states-{}{suffix}", look.tag()),
            );
            states::assert_marks(&rt);
        }
    }
}
