//! The actual Rune window and embedded client: advertise CHECKOUT, capture
//! outbound commands, and feed their correlated replies through the runtime.

use std::sync::Arc;

use phux_client_runtime::Client;
use phux_client_runtime::control::Status;
use phux_protocol::caps::{ServerCapabilities, ServerFeatureExt, ServerFeatureExtSet};
use phux_protocol::checkout::{
    CheckoutAnswer, CheckoutArea, CheckoutDiff, CheckoutEntry, CheckoutRefusal,
    CheckoutRefusalCode, CheckoutRequest, CheckoutStatus,
};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};
use rune::{Env, Key, KeyEvent, Mods, Runtime};

use crate::desk::{Desk, Shown};
use crate::features::changes::presentation::Line;
use crate::keys::Shortcut;

fn fixture(checkout: bool) -> (Arc<Client>, Runtime<Desk>) {
    let caps = if checkout {
        ServerCapabilities::new()
            .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Checkout]))
    } else {
        ServerCapabilities::new()
    };
    let replica = crate::test_support::terminal::Replica::with_capabilities(120, 40, 1, caps);
    let client = replica.client;
    let terminal = replica.terminal;
    let mut desk = crate::test_support::window();
    desk.connection.client = Some(Arc::clone(&client));
    desk.connection.status = Status::Attached;
    // Replica starts with its activity drained. Adopt the retained inventory
    // before polling so reconnect reconciliation can reopen the shown view.
    desk.connection.topology = client.topology();
    desk.connection.layout = Some(phux_client_core::layout::LayoutNode::Leaf(terminal.clone()));
    desk.connection.shown = Some(Shown {
        view: Some(replica.views[0]),
        resource: terminal,
    });
    let mut rt = Runtime::new(desk, crate::ui::view, (1100.0, 720.0), Env::new(true, 2.0));
    rt.on_key = Some(Desk::key);
    rt.frame(0.0);
    poll(&mut rt);
    assert!(
        rt.state.connection.shown.is_some(),
        "fixture has a live focused pane"
    );
    let _ = client.take_outbound();
    (client, rt)
}

fn entry(path: &str, area: CheckoutArea) -> CheckoutEntry {
    CheckoutEntry {
        path: path.to_owned(),
        from: None,
        area,
        code: "M".into(),
        added: Some(1),
        deleted: Some(0),
        binary: false,
    }
}

fn status(entries: Vec<CheckoutEntry>) -> CheckoutAnswer {
    CheckoutAnswer::Status(CheckoutStatus {
        branch: Some("main".into()),
        entries,
        ..CheckoutStatus::default()
    })
}

fn diff(path: &str, area: CheckoutArea) -> CheckoutAnswer {
    CheckoutAnswer::Diff(CheckoutDiff {
        path: path.to_owned(),
        area,
        text: "@@ -0,0 +1 @@\n+hello\n".into(),
        truncated: false,
        binary: false,
    })
}

/// Check all outbound traffic: this read-only sheet may only query a
/// checkout or fit its terminal, never send input or a mutating command.
fn requests(client: &Client) -> Vec<(u32, CheckoutRequest)> {
    client
        .take_outbound()
        .iter()
        .filter_map(|bytes| match FrameKind::decode(bytes).expect("frame").0 {
            FrameKind::Command {
                request_id,
                command:
                    Command::Checkout {
                        terminal_id,
                        request,
                    },
            } => {
                assert_eq!(terminal_id, ResourceId::local(1));
                assert!(matches!(
                    request,
                    CheckoutRequest::Status | CheckoutRequest::Diff { .. }
                ));
                assert_ne!(request_id, 0);
                Some((request_id, request))
            }
            FrameKind::ResizeTerminal { .. } => None,
            frame => panic!("unexpected outbound frame: {frame:?}"),
        })
        .collect()
}

fn request(client: &Client, expected: CheckoutRequest) -> u32 {
    let requests = requests(client);
    let [(request_id, actual)] = requests.as_slice() else {
        panic!("expected one request, got {requests:?}");
    };
    assert_eq!(actual, &expected);
    *request_id
}

fn diff_request(client: &Client, path: &str, area: CheckoutArea) -> u32 {
    request(
        client,
        CheckoutRequest::Diff {
            path: path.to_owned(),
            area,
        },
    )
}

fn reply(client: &Client, request_id: u32, answer: CheckoutAnswer) {
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::Checkout(Box::new(answer))),
        })
        .expect("checkout reply");
}

fn poll(rt: &mut Runtime<Desk>) {
    rt.update(|desk, tx| {
        desk.poll(0.0, tx);
    });
    rt.frame(0.0);
}

fn open(rt: &mut Runtime<Desk>) {
    rt.update(|desk, tx| desk.shortcut(Shortcut::Changes, tx));
    rt.frame(0.0);
    assert!(rt.state.connection.changes.open);
    assert!(rt.find("changes").is_some());
}

fn press(rt: &mut Runtime<Desk>, key: Key) {
    rt.key(KeyEvent {
        key,
        mods: Mods::default(),
    });
    rt.frame(0.0);
}

#[test]
fn the_panel_lists_files_follows_arrows_and_closes_on_escape() {
    use CheckoutArea::{Staged, Unstaged, Untracked};
    let (client, mut rt) = fixture(true);
    open(&mut rt);
    assert!(rt.state.connection.changes.listing.is_none(), "loading");
    let id = request(&client, CheckoutRequest::Status);
    reply(
        &client,
        id,
        status(vec![
            entry("staged.txt", Staged),
            entry("tracked.txt", Unstaged),
            entry("new.txt", Untracked),
        ]),
    );
    poll(&mut rt);
    assert_eq!(rt.state.connection.changes.rows().len(), 3);
    assert!(rt.find("changes-row-2").is_some());
    assert!(rt.find("changes-summary").is_some());
    let stale = diff_request(&client, "staged.txt", Staged);

    press(&mut rt, Key::Down);
    assert_eq!(rt.state.connection.changes.picked, 1);
    let tracked = diff_request(&client, "tracked.txt", Unstaged);
    press(&mut rt, Key::Up);
    let staged = diff_request(&client, "staged.txt", Staged);
    press(&mut rt, Key::Up);
    assert_eq!(rt.state.connection.changes.picked, 2, "wraps");
    let current = diff_request(&client, "new.txt", Untracked);
    press(&mut rt, Key::Text("x".into()));
    assert!(requests(&client).is_empty(), "typing stops at the sheet");

    reply(&client, stale, diff("staged.txt", Staged));
    reply(&client, tracked, diff("tracked.txt", Unstaged));
    reply(&client, staged, diff("staged.txt", Staged));
    poll(&mut rt);
    assert!(
        rt.state.connection.changes.diff.is_none(),
        "stale replies do not paint"
    );
    reply(&client, current, diff("new.txt", Untracked));
    poll(&mut rt);
    let shown = rt
        .state
        .connection
        .changes
        .diff
        .as_ref()
        .expect("diff")
        .as_ref()
        .expect("text");
    assert_eq!(shown.runs[1], (Line::Added, "+hello".into()));
    assert!(rt.find("changes-diff").is_some());

    press(&mut rt, Key::Down);
    let id = diff_request(&client, "staged.txt", Staged);
    reply(
        &client,
        id,
        CheckoutAnswer::Refused(CheckoutRefusal {
            code: CheckoutRefusalCode::ReadFailed,
            message: String::new(),
        }),
    );
    poll(&mut rt);
    assert!(
        rt.state
            .connection
            .changes
            .diff
            .as_ref()
            .expect("answer")
            .as_ref()
            .is_err_and(|words| words.contains("could not read"))
    );
    press(&mut rt, Key::Escape);
    assert!(!rt.state.connection.changes.open);
    assert!(rt.find("changes").is_none());
    assert!(requests(&client).is_empty());
}

#[test]
fn refresh_keeps_the_picked_path_and_area_then_handles_a_clean_checkout() {
    use CheckoutArea::{Staged, Unstaged};
    let (client, mut rt) = fixture(true);
    open(&mut rt);
    let id = request(&client, CheckoutRequest::Status);
    reply(
        &client,
        id,
        status(vec![
            entry("a", Staged),
            entry("b", Staged),
            entry("a", Unstaged),
        ]),
    );
    poll(&mut rt);
    let _ = diff_request(&client, "a", Staged);
    press(&mut rt, Key::Up);
    let old_diff = diff_request(&client, "a", Unstaged);
    press(&mut rt, Key::Text("r".into()));
    let current = request(&client, CheckoutRequest::Status);
    reply(&client, old_diff, diff("a", Unstaged));
    poll(&mut rt);
    assert!(
        rt.state.connection.changes.listing.is_none(),
        "still loading"
    );
    assert!(
        rt.state.connection.changes.diff.is_none(),
        "old diff ignored during refresh"
    );
    reply(
        &client,
        current,
        status(vec![entry("a", Staged), entry("a", Unstaged)]),
    );
    poll(&mut rt);
    assert_eq!(
        rt.state.connection.changes.picked, 1,
        "same path AND area, not index"
    );
    let old = diff_request(&client, "a", Unstaged);
    press(&mut rt, Key::Text("r".into()));
    let superseded = request(&client, CheckoutRequest::Status);
    press(&mut rt, Key::Text("r".into()));
    let id = request(&client, CheckoutRequest::Status);
    reply(&client, old, diff("a", Unstaged));
    reply(&client, superseded, status(vec![]));
    poll(&mut rt);
    assert!(
        rt.state.connection.changes.listing.is_none(),
        "stale status ignored"
    );
    assert!(
        rt.state.connection.changes.diff.is_none(),
        "stale diff ignored"
    );
    reply(&client, id, status(vec![]));
    poll(&mut rt);
    assert!(rt.state.connection.changes.rows().is_empty());
    assert!(rt.state.connection.changes.diff.is_none());
    assert!(rt.find("changes-message").is_some(), "No changes");
    assert!(
        requests(&client).is_empty(),
        "a clean checkout needs no diff"
    );
}

#[test]
fn closed_and_reopened_panels_ignore_the_previous_requests() {
    let (client, mut rt) = fixture(true);
    open(&mut rt);
    let closed = request(&client, CheckoutRequest::Status);
    press(&mut rt, Key::Escape);
    reply(
        &client,
        closed,
        status(vec![entry("old", CheckoutArea::Staged)]),
    );
    poll(&mut rt);
    assert!(rt.state.connection.changes.listing.is_none());
    assert!(requests(&client).is_empty());
    open(&mut rt);
    let old = request(&client, CheckoutRequest::Status);
    press(&mut rt, Key::Escape);
    open(&mut rt);
    let current = request(&client, CheckoutRequest::Status);
    reply(
        &client,
        old,
        status(vec![entry("old", CheckoutArea::Staged)]),
    );
    poll(&mut rt);
    assert!(rt.state.connection.changes.listing.is_none());
    reply(&client, current, status(vec![]));
    poll(&mut rt);
    assert!(rt.state.connection.changes.rows().is_empty());
    assert!(requests(&client).is_empty());
}

#[test]
fn unsupported_and_disconnected_clients_explain_why_nothing_was_sent() {
    let (client, mut rt) = fixture(false);
    open(&mut rt);
    assert!(
        rt.state
            .connection
            .changes
            .listing
            .as_ref()
            .expect("answer")
            .as_ref()
            .is_err_and(|words| words.contains("doesn't support changes"))
    );
    assert!(rt.find("changes-message").is_some());
    assert!(requests(&client).is_empty());
    rt.update(|desk, _| {
        desk.connection.client = None;
        desk.connection.status = Status::Idle;
        desk.refresh_changes();
    });
    assert!(
        rt.state
            .connection
            .changes
            .listing
            .as_ref()
            .expect("answer")
            .as_ref()
            .is_err_and(|words| words.contains("Not connected"))
    );
}

#[test]
fn runtime_failures_and_unknown_areas_are_shown_without_an_invalid_diff_request() {
    let (client, mut rt) = fixture(true);
    open(&mut rt);
    let id = request(&client, CheckoutRequest::Status);
    client
        .feed(phux_wire_fixtures::command_err(
            id,
            ErrorCode::PermissionDenied,
            "denied",
        ))
        .expect("failed checkout");
    poll(&mut rt);
    assert!(
        rt.state
            .connection
            .changes
            .listing
            .as_ref()
            .expect("answer")
            .as_ref()
            .is_err_and(|words| words.contains("Permission denied"))
    );
    assert!(requests(&client).is_empty());
    press(&mut rt, Key::Text("r".into()));
    let id = request(&client, CheckoutRequest::Status);
    reply(
        &client,
        id,
        status(vec![entry("future", CheckoutArea::Unknown)]),
    );
    poll(&mut rt);
    assert!(
        rt.state
            .connection
            .changes
            .diff
            .as_ref()
            .expect("answer")
            .as_ref()
            .is_err_and(|words| words.contains("can't show that kind"))
    );
    assert!(requests(&client).is_empty());
    rt.update(|desk, tx| {
        desk.pick_change(0, tx);
        desk.pick_change(9, tx);
    });
    assert_eq!(rt.state.connection.changes.picked, 0);
    assert!(
        requests(&client).is_empty(),
        "repeated and invalid picks do nothing"
    );
}
