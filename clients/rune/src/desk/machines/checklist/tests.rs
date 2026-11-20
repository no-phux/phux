//! What works here: the document, the words, and the sheet's toggle.

use super::*;

#[test]
fn reads_items_verbatim_and_unknown_words_as_unknown() {
    let doc = serde_json::json!({
        "schema_version": 1,
        "items": [
            {"id": "restore", "title": "Restore after restart", "status": "off",
             "reason": "server not supervised", "fix_command": "phux service install --adopt\n\n"},
            {"id": "notifications", "title": "Asks and notifications", "status": "off",
             "reason": "no phone", "fix_command": null, "fix_action": "enable_notifications"},
            {"id": "remote", "title": "Remote reach", "status": "waiting", "reason": "binding"},
            {"id": "future", "title": "Something new", "status": "sideways", "reason": "?"},
            {"title": "no id"}
        ]
    });
    let items = parse(&doc);
    assert_eq!(items.len(), 4, "an item without an id is dropped");
    assert_eq!(
        items[0].fix.as_deref(),
        Some("phux service install --adopt")
    );
    assert_eq!(items[0].verdict, Verdict::Off);
    assert_eq!(items[1].fix, None, "a phone action is not a desktop fix");
    assert_eq!(items[2].verdict, Verdict::Waiting);
    assert_eq!(items[3].verdict, Verdict::Unknown);
    assert_eq!(items[3].title, "Something new");
    assert!(parse(&serde_json::json!({})).is_empty());
}

#[test]
fn a_fix_pane_never_reuses_a_session() {
    assert_eq!(fresh_name("fix-restore", &[]), "fix-restore");
    let taken = ["fix-restore".to_owned(), "fix-restore-2".to_owned()];
    assert_eq!(fresh_name("fix-restore", &taken), "fix-restore-3");
}

#[test]
fn the_fix_runs_then_leaves_a_shell() {
    let argv = running("a\nb");
    assert_eq!(argv[..2], ["/bin/sh", "-c"]);
    assert_eq!(argv[2], "a\nb\nexec \"${SHELL:-/bin/sh}\"");
}

#[test]
fn a_disconnected_window_says_so_and_the_row_toggles() {
    let mut desk = Desk::open(&crate::launch::Resolved::offline("not launched"), None);
    desk.toggle_checklist(None);
    let deadline = Instant::now() + Duration::from_secs(5);
    let answer = loop {
        if desk.pump_checklist() {
            break desk
                .machines
                .checklist
                .as_ref()
                .and_then(|c| c.answer.clone());
        }
        assert!(Instant::now() < deadline, "the worker answers");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(answer, Some(Err(words(&ChecklistError::NotConnected))));
    desk.toggle_checklist(None);
    assert!(
        desk.machines.checklist.is_none(),
        "pressed again, it goes away"
    );
}

#[test]
fn the_palette_opens_this_machines_checklist() {
    let mut desk = Desk::open(&crate::launch::Resolved::offline("not launched"), None);
    desk.what_works_here();
    assert!(desk.machines.open);
    assert_eq!(
        desk.machines.checklist.as_ref().map(|c| c.machine.clone()),
        Some(None)
    );
}
