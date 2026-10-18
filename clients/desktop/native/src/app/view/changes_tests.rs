//! The Changes panel painted in a real window: where it sits, what it lists,
//! and that its command opens and closes it.

use std::path::PathBuf;

use super::*;
use crate::app::changes::View;
use crate::app::{AppOptions, desk::Link};
use crate::connection::{Connection, DeterministicDial};
use crate::git::{Area, Branch, Entry, Status};
use crate::workspace::{Workspace, WorkspaceOptions};
use gpui::{AnyWindowHandle, Entity, TestAppContext, VisualTestContext};
use gpui_kit::component::Root;

/// A window on a server that never answers, with nothing focused.
fn fixture(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Desk>) {
    let path = std::env::temp_dir().join(format!(
        "phux-changes-{}-{}",
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

/// Settle and paint, so debug bounds describe this frame.
fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.bounds_changed(cx);
            window.draw(cx).clear(cx);
        });
    }
}

fn run(desk: &Entity<Desk>, command: &str, cx: &mut VisualTestContext) {
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.execute(command, window, cx)));
    draw(cx);
}

fn entry(area: Area, path: &str, code: char) -> Entry {
    Entry {
        area,
        path: path.into(),
        from: None,
        code,
    }
}

#[gpui::test]
fn the_changes_panel_opens_beside_the_panes_and_lists_a_checkout(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1200.), px(600.)));
    run(&desk, "changes", cx);
    // No terminal is focused, so there is nothing local to read.
    desk.read_with(cx, |desk, _| {
        assert!(desk.changes.open);
        assert!(matches!(desk.changes.view, View::Unavailable(_)));
    });
    let panel = cx.debug_bounds("changes-panel").expect("the panel paints");
    assert!(panel.right() <= px(1200.));
    assert!(panel.size.width <= px(super::changes::WIDTH));

    desk.update(cx, |desk, cx| {
        desk.changes.view = View::Repo(Status {
            root: PathBuf::from("/tmp/phux"),
            branch: Branch {
                name: Some("main".into()),
                ..Branch::default()
            },
            entries: vec![
                entry(Area::Staged, "src/lib.rs", 'M'),
                entry(Area::Untracked, "notes.txt", '?'),
            ],
        });
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("change-Staged-src/lib.rs").is_some());
    assert!(cx.debug_bounds("change-Untracked-notes.txt").is_some());

    run(&desk, "changes", cx);
    desk.read_with(cx, |desk, _| assert!(!desk.changes.open));
    assert!(cx.debug_bounds("changes-panel").is_none());
}
