//! The chrome's gallery (§5.2): the window's main surfaces with a populated
//! sidebar, painted through Rune's real renderer, on the desktop and the
//! phone, over the design fixture: six themes, the accessibility modes,
//! three locales and a page of forced states. `RUNE_GALLERY_SNAP=DIR` writes
//! each as `DIR/NAME.png`; without it the frames are still built and
//! painted, so a surface that stops drawing fails here.

use rune::{Env, Runtime};

use crate::desk::Desk;
use crate::test_support::server::{self, agent, pane, session, settle};
use crate::theme::Theme;

mod fixture;
mod states;

use fixture::{Locale, Look};

const SCALE: f32 = 2.0;
const DESKTOP: (f32, f32) = (1280.0, 800.0);
const PHONE: (f32, f32) = (393.0, 852.0);

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
        session(1, t("phux"), 1),
        session(2, t("website"), 2),
        session(3, t("phux-mobile"), 3),
    );
    let panes = vec![
        pane(1, &phux, t("restyle sidebar"), Some("~/workspace/phux")),
        pane(2, &phux, t("cargo test"), Some("~/workspace/phux")),
        pane(3, &phux, t("review #2617"), Some("~/workspace/phux")),
        pane(4, &site, t("docs build"), Some("~/workspace/website")),
        pane(5, &site, "zsh", Some("~/workspace/website")),
        pane(6, &mobile, t("ios snapshots"), Some("~/workspace/phux-mobile")),
        pane(7, &mobile, t("android build"), Some("~/workspace/phux-mobile")),
    ];
    let mut topology = crate::desk::topology(vec![phux, site, mobile], panes);
    server::terminal_resources(&mut topology);
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

/// The frame as the app shows it, under `RUNE_GALLERY_SNAP`.
fn paint(gpu: &rune::render::Gpu, rt: &mut Runtime<Desk>, size: (f32, f32), name: &str) -> Vec<u8> {
    server::paint(gpu, rt, size, SCALE, "RUNE_GALLERY_SNAP", name)
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

/// §1.5 nesting in the real tree: the palette's rows are inset in its
/// panel, their radius the panel's less the inset.
#[test]
fn the_palettes_rows_nest_inside_its_panel() {
    let mut rt = window(DESKTOP, true, None, Locale::En);
    rt.update(|d, _| d.toggle_palette());
    settle(&mut rt);
    let panel = crate::ui::element(&rt, "palette");
    let row = crate::ui::element(&rt, "palette-row-0");
    let results = crate::ui::element(&rt, "palette-results");
    let inset = row.rect[0] - results.rect[0];
    let tokens = crate::ui::tokens_for(true, None);
    assert!(
        (row.radius - tokens.nested(panel.radius, inset)).abs() < 0.01,
        "row {} in panel {} at {inset}",
        row.radius,
        panel.radius
    );
}

/// Under reduced motion a modal opens and closes in place: no scale on
/// its first frame, gone on the frame it closes (§1.11).
#[test]
fn reduced_motion_opens_and_closes_without_movement() {
    let mut rt = window(DESKTOP, true, None, Locale::En);
    rt.set_system_reduced_motion(true);
    settle(&mut rt);
    rt.update(|d, _| d.toggle_palette());
    rt.frame(3.016);
    let panel = crate::ui::element(&rt, "palette");
    assert!((panel.scale - 1.0).abs() < 1e-3, "scale {}", panel.scale);
    assert!(!panel.moving, "the palette moved under reduced motion");
    rt.update(|d, _| d.toggle_palette());
    rt.frame(3.032);
    assert!(
        rt.elements()
            .iter()
            .all(|e| e.key.as_deref() != Some("palette") || e.exiting),
        "the palette lingered under reduced motion"
    );
}

/// §1.9's budget over the gallery's surfaces with glass at its fullest:
/// at most four blurs on the desktop, the modal's among them, none inside
/// a one-layer ancestor; none at all under reduce transparency.
#[test]
fn glass_stays_within_its_budget() {
    let pages: [Page; 4] = [
        ("window", |_| {}),
        ("palette", Desk::toggle_palette),
        ("inbox", Desk::toggle_inbox),
        ("find", Desk::open_find),
    ];
    let mut drawn = 0;
    for look in [Look::HouseDark, Look::HouseLight] {
        for (page, open) in pages {
            let mut rt = window(DESKTOP, look.dark(), look.theme(), Locale::En);
            rt.update(|d, _| {
                d.settings.glass = 100;
                open(d);
            });
            settle(&mut rt);
            let n = crate::ui::glass_panels(&rt);
            assert!(n <= 4, "{page}-{}: {n} blurs", look.tag());
            // And as the renderer sees it: every backdrop blur drawn.
            let drawn_blurs = (rt.instances.iter())
                .filter(|i| i.params[1] == rune::render::KIND_BACKDROP_BLUR)
                .count();
            assert!(drawn_blurs <= 4, "{page}: {drawn_blurs} blurs drawn");
            drawn += n;
            rt.update(|d, _| {
                d.set_reduce_transparency(true);
            });
            settle(&mut rt);
            assert_eq!(
                crate::ui::glass_panels(&rt),
                0,
                "{page}: reduce transparency draws glass"
            );
        }
    }
    assert!(drawn > 0, "full glass drew no glass panel at all");
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
    // Focus is Rune's own ring on every focusable control, not one the
    // page paints: forced on, as DevTools forces `:focus-visible`.
    for look in [Look::HouseDark, Look::HouseLight] {
        let mut rt = states::focus_page(look.dark(), look.theme());
        paint(
            &gpu,
            &mut rt,
            states::SIZE,
            &format!("states-focus-{}", look.tag()),
        );
    }
    for look in Look::ALL {
        for (more, suffix) in [(false, ""), (true, "-contrast-more")] {
            let mut rt = states::page(look.dark(), look.theme());
            if more {
                rt.set_system_increased_contrast(true);
            }
            let pixels = paint(
                &gpu,
                &mut rt,
                states::SIZE,
                &format!("states-{}{suffix}", look.tag()),
            );
            states::assert_marks(&rt);
            let tokens = crate::ui::Tokens::resolve(&crate::ui::tokens::Inputs {
                theme: look.theme().copied(),
                increased_contrast: more,
                ..crate::ui::tokens::Inputs::system(look.dark())
            });
            states::assert_invalid_shows(&rt, &pixels, &tokens);
        }
    }
}
