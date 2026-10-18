//! Default-layout persistence and legacy-layout migration journeys.

use super::*;

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI"]
fn rename_save_close_and_relaunch_preserves_default_tabs_and_splits(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    let handle = server.launch_named(&desktop, "journey-A", true, cx);
    let original = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&original, cx) && placement_count(&original, cx) == 1
    });
    cx.update_window(*handle, |_, window, cx| {
        original.update(cx, |desk, cx| desk.execute("split-right", window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| placement_count(&original, cx) == 2);
    cx.update_window(*handle, |_, window, cx| {
        original.update(cx, |desk, cx| desk.execute("new", window, cx));
    })
    .unwrap();
    wait_for(cx, |cx| placement_count(&original, cx) == 3);
    server.run(&["rename", "journey-A", "renamed-A"]);
    wait_for(cx, |cx| {
        original.read_with(cx, |desk, _| desk.session == "renamed-A")
    });
    let expected = snapshot(&original, cx);
    assert_eq!(expected.tabs.len(), 2);
    original.update(cx, |desk, cx| desk.prepare_close(cx));
    let saved_path = desktop.read_with(cx, |desktop, _| desktop.layout.clone());
    assert!(saved_path.exists());
    cx.update_window(*handle, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();

    let reopened = cx.update(Desktop::install);
    server.launch_named(&reopened, "renamed-A", true, cx);
    let restored = reopened.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&restored, cx) && placement_count(&restored, cx) == 3
    });
    assert_eq!(
        reopened.read_with(cx, |desktop, _| desktop.layout.clone()),
        saved_path
    );
    assert_eq!(snapshot(&restored, cx).tabs, expected.tabs);
    restored.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI and disposable XDG_STATE_HOME"]
fn unchanged_legacy_layout_is_materialized_once_before_rename_or_close(cx: &mut TestAppContext) {
    use crate::workspace::persist::{
        FileLayoutStore, layout_paths, layout_value, load_layout, session_layout_paths,
    };
    use std::os::unix::fs::MetadataExt;

    let server = Server::new();
    let initial = desktop(cx);
    let initial_window = server.launch(&initial, cx);
    let first = initial.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&first, cx) && placement_count(&first, cx) == 1
    });
    let saved = snapshot(&first, cx);
    let key = home_key(&first, cx).unwrap();
    let socket = server.socket.display().to_string();
    let named = layout_paths(&socket, "journey-A").layout;
    FileLayoutStore::new(&named, None)
        .write(&layout_value(&saved))
        .unwrap();
    let named_bytes = std::fs::read(&named).unwrap();
    first.update(cx, |desk, cx| desk.prepare_close(cx));
    cx.update_window(*initial_window, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();

    let migrated = cx.update(Desktop::install);
    let migrated_window = server.launch_named(&migrated, "journey-A", true, cx);
    let desk = migrated.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    let canonical = session_layout_paths(&socket, key).layout;
    wait_for(cx, |cx| attached(&desk, cx) && canonical.exists());
    assert_eq!(
        load_layout(&FileLayoutStore::new(&canonical, None).read())
            .initial
            .unwrap()
            .tabs,
        saved.tabs
    );
    let inode = std::fs::metadata(&canonical).unwrap().ino();
    desk.update(cx, |desk, _| {
        for _ in 0..5 {
            desk.persist_if_changed();
        }
    });
    assert_eq!(
        std::fs::metadata(&canonical).unwrap().ino(),
        inode,
        "unchanged presentation does not rewrite the canonical file"
    );
    assert_eq!(
        std::fs::read(&named).unwrap(),
        named_bytes,
        "legacy source is preserved"
    );
    server.run(&["rename", "journey-A", "renamed-A"]);
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| desk.session == "renamed-A")
    });
    // Model a crash's no-save boundary: the on-close hook must not rescue the
    // migration. The canonical file already exists before any close or edit.
    desk.update(cx, |desk, _| desk.persist_writes = false);
    cx.update_window(*migrated_window, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    let relaunched = cx.update(Desktop::install);
    server.launch_named(&relaunched, "renamed-A", true, cx);
    let restored = relaunched.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&restored, cx) && placement_count(&restored, cx) == 1
    });
    assert_eq!(snapshot(&restored, cx).tabs, saved.tabs);
    restored.update(cx, |desk, cx| desk.prepare_close(cx));
}
