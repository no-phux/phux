//! The Changes panel painted in a real window: where it sits, what it lists,
//! how the diff follows the tree, and that its edge resizes it.

use std::path::PathBuf;

use super::*;
use crate::app::changes::View;
use crate::app::{AppOptions, desk::Link};
use crate::connection::{Connection, DeterministicDial};
use crate::git::{Area, Branch, Entry, Source, Status};
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

const CHANGES: &str = "diff --git a/src/app/view.rs b/src/app/view.rs\nindex 1..2 100644\n--- a/src/app/view.rs\n+++ b/src/app/view.rs\n@@ -1,2 +1,2 @@\n fn render() {\n-    old();\n+    new();\n";
const STAGED: &str = "diff --git a/src/lib.rs b/src/lib.rs\nindex 1..2 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1,2 @@\n pub mod app;\n+pub mod model;\n";

fn checkout() -> Status {
    Status {
        root: PathBuf::from("/tmp/phux"),
        branch: Branch {
            name: Some("main".into()),
            ..Branch::default()
        },
        entries: vec![
            entry(Area::Staged, "src/lib.rs", 'M'),
            entry(Area::Unstaged, "src/app/view.rs", 'M'),
            entry(Area::Untracked, "notes.txt", '?'),
        ],
    }
}

fn shown_paths(desk: &Entity<Desk>, cx: &mut VisualTestContext) -> Vec<String> {
    desk.read_with(cx, |desk, cx| {
        let (state, _) = desk.changes.diff.as_ref().expect("a diff view");
        state
            .read(cx)
            .files()
            .iter()
            .map(|file| file.path().to_string())
            .collect()
    })
}

#[gpui::test]
fn the_changes_panel_opens_beside_the_panes_and_lists_a_checkout(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1600.), px(700.)));
    run(&desk, "changes", cx);
    // No terminal is focused, so there is nothing local to read.
    desk.read_with(cx, |desk, _| {
        assert!(desk.changes.open);
        assert!(matches!(desk.changes.view, View::Unavailable(_)));
    });
    let panel = cx.debug_bounds("changes-panel").expect("the panel paints");
    assert!(panel.right() <= px(1600.));
    assert!(panel.size.width <= px(crate::app::changes::WIDTH));

    desk.update(cx, |desk, cx| {
        desk.show_checkout(checkout(), CHANGES, STAGED, cx);
    });
    draw(cx);
    // A tree: `src/app` folds its single-folder chain; files sit under folders.
    assert!(cx.debug_bounds("changes-folder-changes:src/app").is_some());
    assert!(cx.debug_bounds("change-Unstaged-src/app/view.rs").is_some());
    assert!(cx.debug_bounds("change-Untracked-notes.txt").is_some());
    assert!(cx.debug_bounds("change-Staged-src/lib.rs").is_some());
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.changes.source, Source::Changes)
    });
    assert_eq!(shown_paths(&desk, cx), ["src/app/view.rs"]);

    // Revealing a staged file switches the diff to the staged comparison.
    desk.update(cx, |desk, cx| {
        desk.reveal_change(Source::Staged, "src/lib.rs", cx);
    });
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.changes.source, Source::Staged)
    });
    assert_eq!(shown_paths(&desk, cx), ["src/lib.rs"]);

    // Viewed folds the file and counts toward progress, until it changes again.
    desk.update(cx, |desk, cx| desk.toggle_viewed("src/lib.rs", cx));
    desk.read_with(cx, |desk, cx| {
        assert_eq!(desk.changes.progress(), (1, 1));
        let (state, _) = desk.changes.diff.as_ref().unwrap();
        assert!(state.read(cx).is_file_collapsed("src/lib.rs"));
    });
    let edited = STAGED.replace("pub mod model;", "pub mod models;");
    desk.update(cx, |desk, cx| {
        desk.show_checkout(checkout(), CHANGES, &edited, cx);
    });
    desk.read_with(cx, |desk, cx| {
        assert_eq!(desk.changes.progress(), (0, 1));
        let (state, _) = desk.changes.diff.as_ref().unwrap();
        assert!(!state.read(cx).is_file_collapsed("src/lib.rs"));
    });

    // Folding a folder hides its files.
    desk.update(cx, |desk, cx| desk.toggle_folder("changes:src/app", cx));
    draw(cx);
    assert!(cx.debug_bounds("change-Unstaged-src/app/view.rs").is_none());

    let old_diff = desk.read_with(cx, |desk, _| {
        desk.changes.diff.as_ref().unwrap().0.downgrade()
    });
    run(&desk, "changes", cx);
    desk.read_with(cx, |desk, _| {
        assert!(!desk.changes.open);
        assert!(desk.changes.diff.is_none());
        for source in [Source::Changes, Source::Staged] {
            assert!(desk.changes.loaded(source).files.is_empty());
            assert!(desk.changes.loaded(source).sections.is_empty());
        }
    });
    assert!(
        old_diff.upgrade().is_none(),
        "closing must release the diff entity"
    );
    assert!(cx.debug_bounds("changes-panel").is_none());
}

#[gpui::test]
fn changes_poll_uses_the_window_clock_and_stops_when_the_panel_closes(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    run(&desk, "changes", cx);
    desk.update(cx, |desk, cx| {
        desk.show_checkout(checkout(), CHANGES, STAGED, cx)
    });
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert!(matches!(desk.changes.view, View::Repo(_)))
    });

    // Advancing GPUI's clock, not the wall clock, admits the scheduled read.
    // With no focused terminal the read must return to the unavailable view.
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(4));
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert!(matches!(desk.changes.view, View::Unavailable(_)))
    });

    run(&desk, "changes", cx);
    desk.update(cx, |desk, cx| {
        desk.show_checkout(checkout(), CHANGES, STAGED, cx)
    });
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(4));
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert!(!desk.changes.open);
        assert!(
            matches!(desk.changes.view, View::Repo(_)),
            "closing cancels further reads"
        );
    });
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
fn the_changes_panel_resizes_from_its_left_edge_and_leaves_the_panes_room(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1400.), px(700.)));
    run(&desk, "changes", cx);
    desk.update(cx, |desk, cx| {
        desk.begin_changes_drag(cx);
        desk.drag_move(600., 300., true, cx);
        desk.end_drag(cx);
    });
    draw(cx);
    desk.read_with(cx, |desk, _| assert_eq!(desk.changes.width, 800.));
    let panel = cx.debug_bounds("changes-panel").unwrap();
    assert!(panel.size.width > px(760.) && panel.size.width <= px(800.));

    desk.update(cx, |desk, cx| {
        desk.begin_changes_drag(cx);
        desk.drag_move(0., 300., true, cx);
        desk.end_drag(cx);
    });
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.changes.width, 1400. - crate::app::changes::PANES_MIN);
    });
    draw(cx);
    let panes = cx.debug_bounds("main-column").unwrap();
    assert!(panes.size.width >= px(crate::app::changes::PANES_MIN));

    // A narrower window keeps the panes' room; the panel yields toward its floor.
    for width in [1000., 960.] {
        cx.simulate_resize(gpui::size(px(width), px(600.)));
        draw(cx);
        let panes = cx.debug_bounds("main-column").unwrap();
        assert!(
            panes.size.width >= px(crate::app::changes::PANES_MIN),
            "at {width}"
        );
    }
}

#[gpui::test]
fn the_changes_panel_keeps_the_window_foot_in_every_placement(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(1320.), px(840.)));
    run(&desk, "changes", cx);
    for command in [
        "tabs-bottom",
        "plain-terminal",
        "tabs-top",
        "plain-terminal",
    ] {
        let gutter = desk.read_with(cx, |desk, _| desk.skin().shape.gutter);
        let panel = cx.debug_bounds("changes-panel").unwrap();
        // One gutter above the window's foot, with no strip under it, and
        // level with the cards whenever nothing sits under them.
        assert_eq!(panel.bottom(), px(840. - gutter), "before {command}");
        if desk.read_with(cx, |desk, _| desk.tab_position() != TabPosition::Bottom) {
            assert_eq!(panel.bottom(), cards_bottom(&desk, cx), "before {command}");
        }
        run(&desk, command, cx);
    }
}

#[gpui::test]
fn every_diff_toolbar_action_fits_the_inner_narrow_column(cx: &mut TestAppContext) {
    use crate::app::changes::review::Comment;
    use gpui_kit::component::diff::{DiffLineRange, DiffSide};

    let _dial = DeterministicDial::arm();
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(gpui::size(px(700.), px(800.)));
    run(&desk, "changes", cx);
    desk.update(cx, |desk, cx| {
        desk.changes.width = 360.;
        desk.show_checkout(checkout(), CHANGES, STAGED, cx);
        desk.changes.comments.push(Comment {
            id: 1,
            source: Source::Changes,
            range: DiffLineRange::new("src/app/view.rs", DiffSide::Modified, 2, 2),
            body: "Keep all review controls reachable".into(),
        });
        cx.notify();
    });
    for source in [Source::Changes, Source::Staged] {
        desk.update(cx, |desk, cx| {
            desk.set_change_source(source, cx);
            let (state, _) = desk.changes.diff.as_ref().unwrap();
            let path = state.read(cx).files()[0].path().clone();
            state.update(cx, |state, cx| {
                state.set_selected_lines(
                    Some(DiffLineRange::new(path, DiffSide::Modified, 2, 2)),
                    cx,
                );
            });
            cx.notify();
        });
        draw(cx);
        let column = cx.debug_bounds("changes-diff-column").unwrap();
        let toolbar = cx.debug_bounds("changes-diff-toolbar").unwrap();
        assert!(
            column.size.width <= px(200.),
            "exercise the inner ~190px column"
        );
        for selector in [
            "changes-source-changes",
            "changes-source-staged",
            "changes-stage-lines",
            "changes-previous",
            "changes-next",
            "changes-send-review",
        ] {
            let action = cx.debug_bounds(selector).expect(selector);
            assert!(
                action.left() >= toolbar.left() + px(8.)
                    && action.right() <= toolbar.right() - px(8.)
                    && action.top() >= toolbar.top()
                    && action.bottom() <= toolbar.bottom(),
                "{source:?} {selector} fits the inner toolbar: {action:?} in {toolbar:?}"
            );
        }
        assert!(toolbar.right() <= column.right());
    }
}

/// The workspace keeps its foot under the cards.
fn cards_bottom(desk: &Entity<Desk>, cx: &mut VisualTestContext) -> gpui::Pixels {
    let body = cx.debug_bounds("workspace-body").unwrap();
    let foot = desk.read_with(cx, |desk, _| super::panes::foot(desk, &desk.look()));
    body.bottom() - px(foot)
}
