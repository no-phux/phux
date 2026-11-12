//! The panel on the real Rune runtime over a real embedded client. Its
//! server advertises no `CHECKOUT`, so answers are handed in directly.

use phux_client_runtime::control::{CheckoutOutcome, CheckoutResponse, Status};
use phux_protocol::checkout::{
    CheckoutAnswer, CheckoutArea, CheckoutDiff, CheckoutEntry, CheckoutRefusal,
    CheckoutRefusalCode, CheckoutStatus,
};
use rune::{Key, KeyEvent, Mods};

use crate::changes::Line;
use crate::desk::Desk;
use crate::keys::Shortcut;

fn entry(path: &str, area: CheckoutArea, code: &str) -> CheckoutEntry {
    CheckoutEntry {
        path: path.to_owned(),
        from: None,
        area,
        code: code.to_owned(),
        added: Some(1),
        deleted: Some(0),
        binary: false,
    }
}

fn answer(desk: &mut Desk, request_id: u32, answer: CheckoutAnswer) -> bool {
    let terminal = desk.changes.terminal.clone().expect("a terminal");
    desk.changes_answered(&CheckoutResponse {
        request_id,
        terminal_id: terminal,
        outcome: CheckoutOutcome::Answered(Box::new(answer)),
    })
}

fn press(rt: &mut rune::Runtime<Desk>, key: Key) {
    rt.key(KeyEvent {
        key,
        mods: Mods::default(),
    });
    rt.frame(0.0);
}

#[test]
fn the_panel_lists_files_follows_the_arrows_and_closes_on_escape() {
    let (client, mut rt) = crate::grid::budget::input_fixture();
    rt.state.prefs = rune::prefs::Prefs::memory();
    rt.state.status = Status::Attached;
    rt.frame(0.0);
    let _ = client.take_outbound();

    // This server predates CHECKOUT: nothing is sent, and the panel says so.
    rt.update(|d, tx| d.shortcut(Shortcut::Changes, tx));
    rt.frame(0.0);
    assert!(rt.state.changes.open);
    assert!(rt.find("changes").is_some());
    let said = rt.state.changes.listing.clone().expect("an answer");
    assert!(said.is_err_and(|words| words.contains("doesn't support changes")));
    assert!(rt.find("changes-message").is_some());

    // A status arrives for the request that is out.
    rt.update(|d, _| {
        d.changes.listing = None;
        d.changes.status_request = 7;
        let status = CheckoutStatus {
            branch: Some("main".to_owned()),
            entries: vec![
                entry("staged.txt", CheckoutArea::Staged, "A"),
                entry("tracked.txt", CheckoutArea::Unstaged, "M"),
                entry("new.txt", CheckoutArea::Untracked, "?"),
            ],
            ..CheckoutStatus::default()
        };
        assert!(!answer(d, 6, CheckoutAnswer::Status(status.clone())));
        assert!(answer(d, 7, CheckoutAnswer::Status(status)));
    });
    rt.frame(0.0);
    assert_eq!(rt.state.changes.rows().len(), 3);
    assert_eq!(rt.state.changes.picked, 0);
    assert!(rt.find("changes-row-2").is_some());
    assert!(rt.find("changes-summary").is_some());

    // The arrows move through the files and wrap; typing reaches nothing.
    press(&mut rt, Key::Down);
    assert_eq!(rt.state.changes.picked, 1);
    press(&mut rt, Key::Up);
    press(&mut rt, Key::Up);
    assert_eq!(rt.state.changes.picked, 2);
    press(&mut rt, Key::Text("x".to_owned()));

    // The picked file's diff, then a refusal of the next one.
    rt.update(|d, _| {
        d.changes.diff_request = 9;
        let diff = CheckoutDiff {
            path: "new.txt".to_owned(),
            area: CheckoutArea::Untracked,
            text: "@@ -0,0 +1 @@\n+hello\n".to_owned(),
            truncated: false,
            binary: false,
        };
        assert!(answer(d, 9, CheckoutAnswer::Diff(diff)));
    });
    rt.frame(0.0);
    let diff = rt.state.changes.diff.clone().expect("a diff");
    let runs = diff.expect("text").runs;
    assert_eq!(runs[1], (Line::Added, "+hello".to_owned()));
    assert!(rt.find("changes-diff").is_some());
    rt.update(|d, _| {
        d.changes.diff_request = 10;
        let refused = CheckoutRefusal {
            code: CheckoutRefusalCode::ReadFailed,
            message: String::new(),
        };
        assert!(answer(d, 10, CheckoutAnswer::Refused(refused)));
    });
    let refused = rt.state.changes.diff.clone().expect("an answer");
    assert!(refused.is_err_and(|words| words.contains("could not read")));

    press(&mut rt, Key::Escape);
    assert!(!rt.state.changes.open);
    // The window fitting its pane is all that left: no input, no command.
    use phux_protocol::wire::frame::FrameKind;
    for bytes in client.take_outbound() {
        let frame = FrameKind::decode(&bytes).expect("a frame").0;
        assert!(
            matches!(frame, FrameKind::ResizeTerminal { .. }),
            "{frame:?}"
        );
    }
}

#[test]
fn a_refresh_keeps_the_picked_file_and_a_clean_checkout_says_so() {
    let (_client, mut rt) = crate::grid::budget::input_fixture();
    rt.state.prefs = rune::prefs::Prefs::memory();
    rt.update(|d, tx| d.shortcut(Shortcut::Changes, tx));
    // Not attached, so the reason is the connection.
    let said = rt.state.changes.listing.clone().expect("an answer");
    assert!(said.is_err_and(|words| words.contains("Not connected")));

    let status = |paths: &[&str]| CheckoutStatus {
        branch: Some("main".to_owned()),
        entries: paths
            .iter()
            .map(|path| entry(path, CheckoutArea::Unstaged, "M"))
            .collect(),
        ..CheckoutStatus::default()
    };
    rt.update(|d, _| {
        d.changes.status_request = 3;
        answer(d, 3, CheckoutAnswer::Status(status(&["a", "b", "c"])));
        d.changes.picked = 2;
        d.refresh_changes();
        d.changes.status_request = 4;
        answer(d, 4, CheckoutAnswer::Status(status(&["b", "c"])));
    });
    assert_eq!(rt.state.changes.picked, 1, "still on c");

    rt.update(|d, _| {
        d.changes.status_request = 5;
        answer(d, 5, CheckoutAnswer::Status(status(&[])));
    });
    rt.frame(0.0);
    assert!(rt.state.changes.rows().is_empty());
    assert!(rt.state.changes.diff.is_none());
    assert!(rt.find("changes-message").is_some(), "No changes.");
}
