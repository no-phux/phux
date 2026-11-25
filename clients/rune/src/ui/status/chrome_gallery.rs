//! The window chrome's own gallery (lane B): the sidebar with every
//! mark on the ladder, the tab strip, the confirm bar and the split cards,
//! painted through Rune's real renderer in the house look's two modes,
//! under increased contrast and in the +40% pseudo-locale.
//! `RUNE_GALLERY_SNAP=DIR` writes each as `DIR/chrome-*.png`; without it the
//! frames are still built and painted, so a surface that stops drawing
//! fails here.

use phux_client_core::layout::{LayoutNode, SplitDir};
use phux_client_runtime::control::Event;
use phux_protocol::ids::{ResourceId, SessionKey};
use rune::{Env, Runtime};

use crate::desk::{Desk, Pending, Tab};
use crate::test_support::server::{self, agent, pane, session};

mod attention;
mod sidebar_focus;
mod sidebar_hierarchy;

/// The pixel scale every page paints at.
fn scale() -> f32 {
    2.0
}
const SIZE: (f32, f32) = (1280.0, 800.0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Words {
    En,
    /// `en-XA`: accented and padded 40%, so a truncating word shows.
    Long,
}

impl Words {
    fn say(self, s: &str) -> String {
        match self {
            Self::En => s.to_owned(),
            Self::Long => {
                let pad = (s.chars().count() * 2).div_ceil(5);
                let accented: String = s
                    .chars()
                    .map(|c| match c {
                        'a' => 'á',
                        'e' => 'é',
                        'i' => 'í',
                        'o' => 'ö',
                        'u' => 'ü',
                        other => other,
                    })
                    .collect();
                format!("[{accented}{}]", "·".repeat(pad))
            }
        }
    }
}

/// The fixture's sessions, terminals and tabs, named in `words`.
fn named(words: Words) -> (phux_client_runtime::control::Topology, Vec<Tab>) {
    let t = |s: &str| words.say(s);
    let (work, site) = (session(1, t("phux"), 1), session(2, t("website"), 2));
    let names = [
        "restyle sidebar",
        "review",
        "deploy",
        "cargo test",
        "logs",
        "docs build",
        "notes",
        "shell",
    ];
    let panes: Vec<_> = (1..=8u32)
        .zip(names)
        .map(|(id, name)| {
            let home = if id <= 5 { &work } else { &site };
            pane(id, home, t(name), None)
        })
        .collect();
    let mut topology = crate::desk::topology(vec![work, site], panes);
    server::terminal_resources(&mut topology);
    let tab = |id: u8, name: &str, resources: &[u32], active: bool, locked: bool| Tab {
        id: [id; 16],
        name: t(name),
        resources: resources.iter().copied().map(ResourceId::local).collect(),
        active,
        locked,
    };
    let tabs = vec![
        tab(1, "build", &[1, 2], true, false),
        tab(2, "review", &[3], false, false),
        tab(3, "deploy", &[4], false, true),
    ];
    (topology, tabs)
}

/// Rename everything in `rt` into `words`; the marks stay, keyed by id.
fn rename(rt: &mut Runtime<Desk>, words: Words) {
    let (topology, tabs) = named(words);
    rt.update(move |d, _| {
        d.connection.topology = Some(topology);
        d.connection.tabs.list = tabs;
    });
}

/// Two sessions whose terminals hold every mark the window draws, two
/// panes split side by side (the keyboard's lit), three tabs, and a close
/// waiting on its confirmation. Two shells are still running commands:
/// [`finish`] ends them, once they have run long enough to report.
fn window(dark: bool) -> Runtime<Desk> {
    let mut replica = crate::test_support::terminal::Replica::new(120, 40, 1);
    replica.feed("\x1b[2J\x1b[H$ cargo test --locked\r\n   Compiling phux-rune\r\n".to_owned());
    let mut desk = crate::test_support::window();
    // The gallery reviews the strip beside the sidebar.
    desk.settings.tab_bar = phux_config_core::rune::TabBar::Top;
    desk.connection.client = Some(std::sync::Arc::clone(&replica.client));
    desk.connection.layout = Some(LayoutNode::Split {
        dir: SplitDir::Horizontal,
        ratio: 0.55,
        left: Box::new(LayoutNode::Leaf(replica.terminal.clone())),
        right: Box::new(LayoutNode::Leaf(ResourceId::local(9))),
    });
    desk.connection.shown = Some(crate::desk::Shown {
        resource: replica.terminal.clone(),
        view: Some(replica.views[0]),
    });
    desk.pending = Some(Pending::Close(ResourceId::local(4)));
    let mut rt = crate::test_support::runtime(desk, crate::ui::view, SIZE, Env::new(dark, scale()));
    rt.frame(0.0);
    rename(&mut rt, Words::En);
    rt.update(move |d, _| {
        let started = |id| Event::CommandStarted {
            terminal_id: ResourceId::local(id),
        };
        for event in [
            agent(1, "claude", "working"),
            agent(2, "codex", "blocked"),
            agent(3, "claude", "done"),
            started(4),
            Event::Bell {
                terminal_id: ResourceId::local(5),
            },
            agent(6, "gemini", "idle"),
            started(7),
            started(8),
        ] {
            d.activities.apply(&event, &[]);
        }
    });
    rt
}

/// End the long commands: one failed, one succeeded, each off screen.
fn finish(rt: &mut Runtime<Desk>) {
    rt.update(|d, _| {
        for (id, code) in [(4, 1), (7, 0)] {
            let event = Event::CommandFinished {
                terminal_id: ResourceId::local(id),
                exit_code: Some(code),
            };
            d.activities.apply(&event, &[]);
        }
    });
}

fn paint(gpu: &rune::render::Gpu, rt: &mut Runtime<Desk>, name: &str) {
    server::paint(gpu, rt, SIZE, scale(), "RUNE_GALLERY_SNAP", name);
}

/// What every page must show: the ladder's rows, the tabs, the danger
/// confirmation and both cards, each still a Tab stop where it acts.
fn assert_chrome(rt: &Runtime<Desk>, tag: &str) {
    for id in 1..=8 {
        let key = format!("pane-{}", ResourceId::local(id));
        assert!(
            crate::ui::sidebar::tests::focusable(rt, &key),
            "{tag}: pane row {id} is focusable"
        );
    }
    for i in 0..3 {
        let tab = crate::ui::element(rt, &format!("tab-{i}"));
        assert!(tab.focusable, "{tag}: tab {i} is focusable");
        assert_eq!(tab.role, "tab", "{tag}");
    }
    assert!(rt.find("confirm-ok").is_some(), "{tag}: the confirmation");
    let divider = crate::ui::element(rt, "divider-[]");
    assert_eq!(divider.role, "separator", "{tag}");
    assert!(
        divider.focusable,
        "{tag}: the divider steps from the keyboard"
    );
    // Every word on the ladder is on screen beside its mark.
    for word in ["working", "needs you", "done", "failed", "bell", "idle"] {
        let shown = (rt.elements().iter()).any(|e| {
            e.text.as_deref().is_some_and(|t| t.ends_with(word)) || e.label.as_deref() == Some(word)
        });
        assert!(shown, "{tag}: {word}");
    }
}

#[test]
fn the_chrome_gallery_paints() {
    let gpu = rune::render::Gpu::headless();
    // One fresh window per page: a runtime's glyphs are uploaded to the
    // renderer that first painted them, and each page paints with its own.
    let mut pages = Vec::new();
    for (dark, mode) in [(true, "dark"), (false, "light")] {
        pages.push((window(dark), format!("chrome-{mode}"), false, Words::En));
        let more = format!("chrome-contrast-more-{mode}");
        pages.push((window(dark), more, true, Words::En));
        let long = format!("chrome-en-XA-{mode}");
        pages.push((window(dark), long, false, Words::Long));
    }
    // A shell's command reports how it ended only once it ran this long.
    std::thread::sleep(std::time::Duration::from_millis(5_100));
    for (rt, name, more, words) in &mut pages {
        finish(rt);
        rename(rt, *words);
        rt.set_system_increased_contrast(*more);
        paint(&gpu, rt, name);
        assert_chrome(rt, name);
    }
}

#[test]
fn confirming_an_end_is_the_danger_button() {
    use super::confirm::destructive;
    let key = SessionKey::new([1; 16]).expect("a key");
    assert!(destructive(&Pending::Close(ResourceId::local(1))));
    assert!(destructive(&Pending::CloseTab(vec![ResourceId::local(1)])));
    assert!(destructive(&Pending::CloseSession(key, "work".to_owned())));
    assert!(!destructive(&Pending::Paste(
        ResourceId::local(1),
        "a\nb".to_owned()
    )));
}
