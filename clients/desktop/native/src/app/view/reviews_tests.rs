//! The Reviews surface painted in a real window, on fixture data: it takes
//! the panes' place, lists the inbox, folds viewed files, and owns its keys.

use super::*;
use crate::app::reviews::Tab;
use crate::app::{AppOptions, desk::Link};
use crate::connection::{Connection, DeterministicDial};
use crate::workspace::{Workspace, WorkspaceOptions};
use gpui::{AnyWindowHandle, Entity, TestAppContext, VisualTestContext};
use gpui_kit::component::Root;

/// A window on a server that never answers.
fn fixture(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Desk>) {
    let path = std::env::temp_dir().join(format!(
        "phux-reviews-{}-{}",
        std::process::id(),
        cx.test_function_name().unwrap_or("isolated")
    ));
    std::fs::create_dir_all(&path).unwrap();
    let handle = cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        crate::app::open(
            AppOptions {
                socket: path.join("never-connected.sock"),
                session: String::new(),
                phux: path.join("not-installed-phux"),
                layout_path: path.join("layout.json"),
                fresh: false,
                initial_terminal: None,
                quick: false,
                dev_telemetry: false,
                startup_error: None,
                cli_install: None,
            },
            cx,
        )
        .unwrap()
    });
    let desk = (*handle)
        .update(cx, |root, _, cx| {
            root.downcast::<Root>()
                .unwrap()
                .read(cx)
                .view()
                .clone()
                .downcast::<Desk>()
                .unwrap()
        })
        .unwrap();
    let connection =
        Connection::connect(path.join("never-connected.sock"), "fixture".into()).unwrap();
    desk.update(cx, |desk, cx| {
        desk.link = Some(Link {
            connection,
            workspace: Workspace::new("fixture".into(), WorkspaceOptions::default()),
        });
        cx.notify();
    });
    (*handle, desk)
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.bounds_changed(cx);
            window.draw(cx).clear(cx);
        });
    }
}

fn seed(desk: &Entity<Desk>, tab: Tab, cx: &mut VisualTestContext) {
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.seed_reviews_fixture(tab, window, cx)));
    draw(cx);
}

#[gpui::test]
fn reviews_take_the_panes_place_with_the_inbox_and_the_diff(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1440.), px(800.)));
    seed(&desk, Tab::Files, cx);

    let surface = cx
        .debug_bounds("reviews-surface")
        .expect("the surface paints");
    assert!(surface.right() <= px(1440.));
    let inbox = cx.debug_bounds("reviews-inbox").expect("the inbox paints");
    assert!(inbox.left() < cx.debug_bounds("reviews-detail").unwrap().left());
    // Grouped by repository: each pull request has its row.
    assert!(cx.debug_bounds("reviews-row-no-phux/phux-1812").is_some());
    assert!(cx.debug_bounds("reviews-row-phall1/phig-24").is_some());
    assert!(cx.debug_bounds("reviews-diff").is_some());
    assert!(cx.debug_bounds("reviews-bar").is_some());

    // GitHub's viewed mark folds the file; the other stays open.
    desk.read_with(cx, |desk, cx| {
        let opened = desk.reviews.opened.as_ref().unwrap();
        assert_eq!(opened.progress(), (1, 2));
        let (state, _) = opened.diff.as_ref().unwrap();
        let state = state.read(cx);
        assert!(state.is_file_collapsed("docs/desktop-reviews.md"));
        assert!(!state.is_file_collapsed("crates/phux-git/src/github/inbox.rs"));
    });
}

#[gpui::test]
fn reviews_own_their_keys_and_escape_closes_them(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1440.), px(800.)));
    seed(&desk, Tab::Files, cx);

    cx.simulate_keystrokes("3");
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.reviews.opened.as_ref().unwrap().tab, Tab::Checks);
    });
    cx.simulate_keystrokes("2");
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.reviews.opened.as_ref().unwrap().tab, Tab::Conversation);
    });

    cx.simulate_keystrokes("escape");
    draw(cx);
    desk.read_with(cx, |desk, _| assert!(!desk.reviews.open));
    assert!(cx.debug_bounds("reviews-surface").is_none());
    // The review in progress survives closing.
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.reviews.opened.as_ref().unwrap().pending.len(), 1);
    });
}

#[gpui::test]
fn a_pane_command_closes_reviews(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    seed(&desk, Tab::Files, cx);
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.execute("split-right", window, cx)));
    draw(cx);
    desk.read_with(cx, |desk, _| assert!(!desk.reviews.open));
}

#[gpui::test]
fn an_unsent_review_comes_back_with_its_pull_request(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    seed(&desk, Tab::Files, cx);
    let first = desk.read_with(cx, |desk, _| desk.reviews.opened_key().cloned().unwrap());

    cx.simulate_keystrokes("j");
    draw(cx);
    desk.read_with(cx, |desk, _| {
        let opened = desk.reviews.opened.as_ref().unwrap();
        assert_ne!(opened.key, first);
        assert!(opened.pending.is_empty());
    });

    cx.simulate_keystrokes("k");
    draw(cx);
    desk.read_with(cx, |desk, _| {
        let opened = desk.reviews.opened.as_ref().unwrap();
        assert_eq!(opened.key, first);
        assert_eq!(opened.pending.len(), 1, "the kept comment is back");
    });
}

#[gpui::test]
fn escape_in_the_summary_leaves_the_field_not_the_surface(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    seed(&desk, Tab::Files, cx);
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.focus_review_summary(window, cx)));
    draw(cx);
    cx.simulate_keystrokes("escape");
    draw(cx);
    desk.read_with(cx, |desk, _| assert!(desk.reviews.open));
    cx.simulate_keystrokes("escape");
    draw(cx);
    desk.read_with(cx, |desk, _| assert!(!desk.reviews.open));
}
