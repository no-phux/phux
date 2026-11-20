//! The overlays and modal surfaces on tokens (§4.1.3, Lane C): painted
//! through Rune's real renderer over a populated window, walked from the
//! keyboard, and recorded by the motion gate. `RUNE_GALLERY_SNAP=DIR`
//! writes each frame as `DIR/overlay-NAME.png`.

use rune::{Env, Key, KeyEvent, Mods, PointerPress, Runtime};

use crate::desk::{Desk, NoticeKind};
use crate::test_support::server::{self, agent, pane, session, settle};
use crate::theme::Theme;

use crate::test_support::SCALE;
mod help;
const DESKTOP: (f32, f32) = (1280.0, 800.0);

const SCREEN: &str = "\x1b[2J\x1b[H\x1b[1;38;5;208m✻\x1b[0m Welcome to \x1b[1mClaude Code\x1b[0m\r\n\r\n\
\x1b[2m> \x1b[0mrestyle the overlays on tokens\r\n\r\n\
\x1b[38;5;245m⏺\x1b[0m Reading \x1b[1mclients/rune/src/ui/overlay.rs\x1b[0m\r\n\
   Compiling phux-rune v0.1.0\r\n\
    Finished `test` profile [unoptimized + debuginfo] target(s) in 41.2s\r\n\
test result: \x1b[32mok\x1b[0m. 1412 passed; 0 failed; 3 ignored\r\n";

/// A window attached to a populated server, the first terminal on screen
/// with output, agents waiting (`waiting`) or none.
fn window(dark: bool, theme: Option<&'static Theme>, waiting: bool) -> Runtime<Desk> {
    let mut replica = crate::test_support::terminal::Replica::new(120, 40, 1);
    replica.feed(SCREEN.to_owned());
    let mut desk = crate::test_support::window();
    desk.settings.theme = theme;
    desk.connection.client = Some(std::sync::Arc::clone(&replica.client));
    desk.connection.layout = Some(phux_client_core::layout::LayoutNode::Leaf(
        replica.terminal.clone(),
    ));
    desk.connection.shown = Some(crate::desk::Shown {
        resource: replica.terminal.clone(),
        view: Some(replica.views[0]),
    });
    let (phux, site) = (session(1, "phux", 1), session(2, "website", 2));
    let panes = vec![
        pane(1, &phux, "restyle overlays", None),
        pane(2, &phux, "cargo test", None),
        pane(3, &phux, "review #2617", None),
        pane(4, &site, "docs build", None),
    ];
    let topology = crate::desk::topology(vec![phux, site], panes);
    let mut rt =
        crate::test_support::runtime(desk, crate::ui::view, DESKTOP, Env::new(dark, SCALE));
    rt.on_key = Some(Desk::key);
    rt.frame(0.0);
    rt.update(move |d, _| {
        d.connection.topology = Some(topology);
        if waiting {
            for event in [
                agent(1, "claude", "working"),
                agent(3, "codex", "blocked"),
                agent(4, "claude", "done"),
            ] {
                d.activities.apply(&event, &[]);
            }
        }
    });
    rt
}

/// The frame as the app shows it, blended in sRGB; written out under
/// `RUNE_GALLERY_SNAP`. The pixels come back for the checks.
fn paint(gpu: &rune::render::Gpu, rt: &mut Runtime<Desk>, name: &str) -> Vec<u8> {
    let file = format!("overlay-{name}");
    server::paint(gpu, rt, DESKTOP, SCALE, "RUNE_GALLERY_SNAP", &file)
}

/// The pixel at window point `(x, y)`.
fn at(pixels: &[u8], x: f32, y: f32) -> [u8; 4] {
    let w = (DESKTOP.0 * SCALE) as usize;
    let (px, py) = ((x * SCALE) as usize, (y * SCALE) as usize);
    let i = (py * w + px) * 4;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

fn distance(a: [u8; 4], b: [u8; 4]) -> u32 {
    (0..3).map(|i| u32::from(a[i].abs_diff(b[i]))).sum()
}

fn press(rt: &mut Runtime<Desk>, key: Key) {
    rt.key(KeyEvent {
        key,
        mods: Mods::default(),
    });
    settle(rt);
}

fn focused(rt: &Runtime<Desk>) -> Option<String> {
    rt.elements()
        .into_iter()
        .find(|e| e.focused)
        .and_then(|e| e.key)
}

/// The node in an accessibility dump whose `field` is `want`.
fn find_by<'a>(node: &'a serde_json::Value, field: &str, want: &str) -> &'a serde_json::Value {
    fn walk<'a>(
        node: &'a serde_json::Value,
        field: &str,
        want: &str,
    ) -> Option<&'a serde_json::Value> {
        if node[field] == want {
            return Some(node);
        }
        node["children"]
            .as_array()?
            .iter()
            .find_map(|child| walk(child, field, want))
    }
    walk(node, field, want).unwrap_or_else(|| panic!("no {field} {want} in the tree"))
}

fn has(node: &serde_json::Value, state: &str) -> bool {
    node["states"]
        .as_array()
        .is_some_and(|states| states.iter().any(|s| s == state))
}

fn open_menu(rt: &mut Runtime<Desk>) {
    rt.update(|d, _| {
        d.pointer.menu = Some(PointerPress { x: 420.0, y: 260.0 });
    });
}

/// The looks the overlays are checked in: the house look both ways, a
/// light theme derived by the solver, and the house light under increased
/// contrast.
const LOOKS: [(&str, bool, Option<&str>, bool); 4] = [
    ("dark", true, None, false),
    ("light", false, None, false),
    ("solarized-light", false, Some("solarized-light"), false),
    ("light-contrast-more", false, None, true),
];

/// One surface opened over a fresh window. Each frame paints from its own
/// runtime: an offscreen renderer per paint starts with an empty glyph
/// atlas, which a runtime that already shaped its text would not refill.
type Open = fn(&mut Desk);

const SURFACES: [(&str, Open); 9] = [
    ("palette", |d| d.toggle_palette()),
    ("palette-matches", |d| {
        d.toggle_palette();
        d.palette_query.replace_text("re");
        d.palette_edited();
    }),
    ("context-menu", |d| {
        d.pointer.menu = Some(PointerPress { x: 420.0, y: 260.0 });
    }),
    ("find", |d| {
        d.open_find();
        d.connection.find_query.replace_text("cargo");
        d.run_search();
    }),
    ("find-miss", |d| {
        d.open_find();
        d.connection.find_query.replace_text("no such words");
        d.run_search();
    }),
    ("toast", |d| {
        d.offer("A new phux is ready to install.", "Show me", |_| {});
    }),
    ("toast-error", |d| {
        d.say(
            NoticeKind::Error,
            "Couldn't reach the host: connection refused.",
        );
    }),
    ("inbox", |d| d.toggle_inbox()),
    ("news", |d| d.toggle_news()),
];

#[test]
fn the_overlay_gallery_paints() {
    let gpu = rune::render::Gpu::headless();
    for (tag, dark, theme, more) in LOOKS {
        for (name, open) in SURFACES {
            let mut rt = window(dark, theme.and_then(crate::theme::find), true);
            rt.set_system_increased_contrast(more);
            settle(&mut rt);
            rt.update(move |d, _| open(d));
            paint(&gpu, &mut rt, &format!("{name}-{tag}"));
        }
    }
    for (tag, dark) in [("dark", true), ("light", false)] {
        let mut rt = window(dark, None, false);
        settle(&mut rt);
        rt.update(|d, _| d.toggle_inbox());
        paint(&gpu, &mut rt, &format!("inbox-empty-{tag}"));
    }
}

/// The palette's edge reads in light mode: `border.base` where the panel
/// meets the scrim, not the old white hairline that vanished there.
#[test]
fn the_light_palette_has_a_visible_border() {
    let gpu = rune::render::Gpu::headless();
    let mut rt = window(false, None, true);
    rt.update(|d, _| d.toggle_palette());
    let pixels = paint(&gpu, &mut rt, "palette-border-light");
    let panel = crate::ui::element(&rt, "palette");
    let [x, y, w, h] = panel.rect;
    let mid = y + h / 2.0;
    let half = 0.5 / SCALE;
    let edge = at(&pixels, x + half, mid);
    let inside = at(&pixels, x + 6.0, mid);
    let outside = at(&pixels, x - 6.0, mid);
    assert!(
        distance(edge, inside) >= 12 && distance(edge, outside) >= 12,
        "edge {edge:?} against inside {inside:?} and outside {outside:?} ({w} wide)"
    );
}

/// The kit menu walked from the keyboard: it opens on its first item,
/// the arrows move through the items, Escape closes it and gives the
/// keyboard back to what had it.
#[test]
fn the_context_menu_walks_from_the_keyboard_and_escape_returns_focus() {
    let mut rt = window(true, None, false);
    settle(&mut rt);
    let before = focused(&rt);
    open_menu(&mut rt);
    settle(&mut rt);
    let menu = crate::ui::element(&rt, "context-menu");
    assert_eq!(menu.role, "menu");
    let items: Vec<_> = (rt.elements().into_iter())
        .filter(|e| e.role == "menuitem")
        .collect();
    assert_eq!(
        items.len(),
        crate::desk::CONTEXT_MENU.len(),
        "one item per action"
    );
    assert!(
        items
            .iter()
            .any(|e| e.label.as_deref() == Some("Copy Selection")),
        "items say their actions"
    );
    let opened = focused(&rt);
    assert!(
        opened
            .as_deref()
            .is_some_and(|k| k.starts_with("context-item")),
        "the menu takes the keyboard: {opened:?}"
    );
    // A pointer opened it: the first arrow shows the keyboard's place.
    press(&mut rt, Key::Down);
    let first = focused(&rt);
    assert!(
        first
            .as_deref()
            .is_some_and(|k| k.starts_with("context-item"))
    );
    press(&mut rt, Key::Down);
    let next = focused(&rt);
    assert!(
        next.as_deref()
            .is_some_and(|k| k.starts_with("context-item"))
            && next != first,
        "Down moves to the next item: {first:?} then {next:?}"
    );
    press(&mut rt, Key::Up);
    assert_eq!(focused(&rt), first, "Up comes back");
    press(&mut rt, Key::Escape);
    assert!(rt.state.pointer.menu.is_none(), "Escape closes it");
    assert!(
        rt.state.modal().is_none(),
        "the terminal has the keys again"
    );
    for _ in 0..60 {
        rt.frame(4.0);
    }
    assert!(rt.find("context-menu").is_none());
    assert_eq!(focused(&rt), before, "focus goes back where it was");
}

/// Every context row shows its whole title: the kit's fixed minimum width
/// once cut "Screenshot to Terminal" to "Screenshot to…".
#[test]
fn the_context_menu_is_wide_enough_for_every_title() {
    let mut rt = window(true, None, false);
    settle(&mut rt);
    open_menu(&mut rt);
    settle(&mut rt);
    let style = crate::ui::tokens_for(true, None).kit.ty.ui_compact;
    let labels: Vec<_> = (rt.elements().into_iter())
        .filter(|e| e.key.as_deref() == Some("label") && e.path.contains("context-item"))
        .collect();
    assert_eq!(labels.len(), crate::desk::CONTEXT_MENU.len());
    for label in labels {
        let words = label.text.clone().unwrap_or_default();
        let (need, _) = rune::text::measure(&words, &[], style, None);
        assert!(
            label.rect[2] + 0.5 >= need,
            "{words:?} is cut: {} of {need}",
            label.rect[2]
        );
    }
}

/// A menu row runs its action and closes the menu; the tab menu disables
/// Close Tab while the tab is locked.
#[test]
fn a_menu_pick_runs_its_action_and_a_locked_tab_cannot_close() {
    let mut rt = window(true, None, false);
    open_menu(&mut rt);
    settle(&mut rt);
    let find = (rt.elements().into_iter())
        .find(|e| e.role == "menuitem" && e.label.as_deref() == Some("Find in Terminal"))
        .and_then(|e| e.key)
        .expect("a Find row");
    let (x, y) = rt.find(&find).unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    settle(&mut rt);
    assert!(rt.state.pointer.menu.is_none());
    assert!(rt.state.connection.find.is_some(), "Find ran");

    rt.update(|d, _| {
        d.connection.tabs.list = vec![crate::desk::Tab {
            id: [7; 16],
            name: "work".into(),
            resources: Vec::new(),
            active: true,
            locked: true,
        }];
        d.pointer.tab_menu = true;
        d.pointer.menu = Some(PointerPress { x: 420.0, y: 260.0 });
    });
    settle(&mut rt);
    let tree = rt.accessibility_json();
    let close = find_by(&tree, "name", "Close Tab");
    assert!(
        has(close, "disabled"),
        "a locked tab refuses to close: {close}"
    );
    let lock = (rt.elements().into_iter())
        .find(|e| e.role == "menuitem" && e.label.as_deref() == Some("Unlock Tab"));
    assert!(lock.is_some(), "the lock row says which way it goes");
}

/// The find bar's controls are kit controls with names, and a query that
/// matches nothing marks the field invalid.
#[test]
fn the_find_bar_names_its_controls_and_marks_a_miss_invalid() {
    let mut rt = window(true, None, false);
    rt.update(|d, _| d.open_find());
    settle(&mut rt);
    let case = crate::ui::element(&rt, "find-case");
    assert_eq!(case.role, "radiogroup");
    assert_eq!(case.label.as_deref(), Some("Match case"));
    for (key, name) in [
        ("find-older", "Older match"),
        ("find-newer", "Newer match"),
        ("find-close", "Close find"),
    ] {
        let button = crate::ui::element(&rt, key);
        assert_eq!(button.role, "button", "{key}");
        assert_eq!(button.label.as_deref(), Some(name), "{key}");
    }
    let tree = rt.accessibility_json();
    assert!(!has(find_by(&tree, "key", "find-query"), "invalid"));
    rt.update(|d, _| {
        d.connection
            .find_query
            .replace_text("no such words anywhere");
        d.run_search();
    });
    settle(&mut rt);
    let tree = rt.accessibility_json();
    assert!(
        has(find_by(&tree, "key", "find-query"), "invalid"),
        "a miss is invalid"
    );
    let case_on = rt.state.connection.find.as_ref().unwrap().case_sensitive;
    let (x, y) = rt.find("segment0").expect("the Aa segment");
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    settle(&mut rt);
    assert_ne!(
        rt.state.connection.find.as_ref().unwrap().case_sensitive,
        case_on,
        "Aa toggles case"
    );
}

/// The toast is a notifications region with a named way to dismiss it,
/// and an error carries the failed mark beside its words.
#[test]
fn the_toast_is_a_region_and_an_error_carries_its_mark() {
    let mut rt = window(true, None, false);
    rt.update(|d, _| d.say(NoticeKind::Error, "Couldn't reach the host."));
    settle(&mut rt);
    let region = crate::ui::element(&rt, "toast-region");
    assert_eq!(region.role, "region");
    assert_eq!(region.label.as_deref(), Some("Notifications"));
    let close = crate::ui::element(&rt, "toast-close");
    assert_eq!(close.label.as_deref(), Some("Dismiss"));
    assert!(
        rt.elements()
            .iter()
            .any(|e| e.role == "alert" && e.text.as_deref() == Some("Couldn't reach the host.")),
        "an error is read at once"
    );
    let words = crate::ui::element(&rt, "toast");
    let toast = rt.presented("toast").unwrap().0;
    assert!(toast[2] > 0.0 && words.rect[3] > 0.0);
}

/// The motion gate over the menu, the drawer and the toast: each opens and
/// closes without a jump, a flicker or a spring that never settles.
#[test]
fn overlays_motion_gate() {
    use rune::motion_gate::{self, Job, Run};

    fn cell(open: fn(&mut Desk), close: fn(&mut Desk)) -> Job {
        Box::new(move || {
            let mut r = Run::new(window(true, None, true), 120.0);
            r.idle(0.5);
            r.step();
            r.act("open", |rt| rt.update(|d, _| open(d)));
            r.play(1.0);
            r.act("close", |rt| rt.update(|d, _| close(d)));
            r.play(1.0);
            r.rec
        })
    }
    fn menu_open(d: &mut Desk) {
        d.pointer.menu = Some(PointerPress { x: 420.0, y: 260.0 });
    }
    fn toast_open(d: &mut Desk) {
        d.say(NoticeKind::Info, "Copied 3 lines");
    }
    motion_gate::gate_rows(
        vec![
            ("menu/open-close".into(), cell(menu_open, Desk::close_menu)),
            (
                "drawer/open-close".into(),
                cell(Desk::toggle_inbox, Desk::close_inbox),
            ),
            (
                "toast/in-out".into(),
                cell(toast_open, Desk::dismiss_notice),
            ),
        ],
        &[],
    );
}
