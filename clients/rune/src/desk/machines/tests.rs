//! The machines sheet: listings, adds, and pairing from a link.

use super::*;

#[test]
fn labels_the_local_server_by_where_it_is() {
    assert_eq!(Machines::new(None, None).label(), machines::LOCAL);
    assert_eq!(Machines::new(Some("mini".into()), None).label(), "mini");
}

#[test]
fn a_listing_moves_the_window_only_after_an_add() {
    let host = |name: &str| Host {
        name: name.to_owned(),
        ssh: None,
        ssh_only: false,
        paused: false,
    };
    let mut list = Machines::new(None, None);
    list.hosts = vec![host("a"), host("b")];
    assert_eq!(list.take_added(1), None);
    list.before = Some(vec![host("a")]);
    list.awaited = Some(2);
    list.request = Some(Add {
        target: "me@b".to_owned(),
        name: None,
        tailnet: false,
        code: None,
    });
    // A listing from before the add finished can't speak for it.
    assert_eq!(list.take_added(1), None);
    assert_eq!(list.take_added(2).as_deref(), Some("b"));
    assert_eq!(list.take_added(2), None);
}

fn desk() -> Desk {
    Desk::open(&crate::launch::Resolved::offline("not launched"), None)
}

#[test]
fn this_machine_folds_and_the_others_open() {
    let mut desk = desk();
    desk.toggle_host(None);
    assert!(desk.machines.folded);
    desk.toggle_host(Some("mini".to_owned()));
    assert!(desk.machines.expanded.contains(&Some("mini".to_owned())));
    desk.toggle_host(Some("mini".to_owned()));
    assert!(desk.machines.expanded.is_empty());
}

#[test]
fn a_slow_listing_cant_undo_a_newer_one() {
    let mut desk = desk();
    let list = |refresh, name: &str| {
        let hosts = vec![Host {
            name: name.to_owned(),
            ssh: None,
            ssh_only: false,
            paused: false,
        }];
        Done::Listed(refresh, Ok(hosts), Vec::new())
    };
    let sender = desk.machines.done.0.clone();
    sender.send(list(2, "new")).unwrap();
    sender.send(list(1, "old")).unwrap();
    desk.pump_machines();
    assert_eq!(desk.machines.hosts[0].name, "new");
}

#[test]
fn an_add_finishing_behind_a_closed_sheet_stays_put_and_offers() {
    let mut desk = desk();
    desk.landed("mini".to_owned());
    assert_eq!(desk.machines.current, None);
    assert_eq!(
        desk.machines.outcome,
        Some(Ok("mini is set up.".to_owned()))
    );
    let notice = desk.notice.as_ref().map(|notice| notice.text.as_str());
    assert_eq!(notice, Some("mini is set up. Switch to it from Machines."));
}

const LINK: &str = "phux://connect?quic=quic://relay.example:4433&sni=mini-route\
    &fp=abababababababababababababababababababababababababababababababab\
    &ca=sha256:abababababababababababababababababababababababababababababababab\
    #token=cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

#[test]
fn a_bad_link_opens_machines_and_says_why() {
    let mut desk = desk();
    desk.pair_from_link("phux://connect?url=wss://mini:8787");
    assert!(desk.machines.open);
    assert!(desk.machines.adding.is_none() && desk.machines.request.is_none());
    let error = desk.machines.error.as_deref().unwrap_or_default();
    assert!(
        error.starts_with("This pairing link can't be used"),
        "{error}"
    );
}

#[test]
fn a_link_pairs_through_the_cli_with_the_sheet_open() {
    let mut desk = desk();
    desk.pair_from_link(LINK);
    assert_eq!(
        desk.machines.error.as_deref(),
        Some("Install the phux CLI to add a machine.")
    );
    desk.machines.phux = Some(PathBuf::from("/nonexistent/phux"));
    desk.pair_from_link(LINK);
    assert!(
        desk.machines.open,
        "landing moves the window only with it open"
    );
    assert_eq!(desk.machines.adding.as_deref(), Some("Pairing mini-route…"));
    let request = desk.machines.request.clone().expect("an add");
    assert_eq!(request.target, "mini-route");
    assert_eq!(request.code.map(|code| code.0).as_deref(), Some(LINK));
    // One at a time: a second link while the first pairs is refused.
    desk.pair_from_link(LINK);
    assert!(
        desk.machines
            .error
            .as_deref()
            .unwrap_or_default()
            .starts_with("Another machine")
    );
}

#[test]
fn a_failed_pairing_says_what_failed() {
    let failure = pairing_failure("phux: error: --code: could not enroll");
    assert_eq!(
        failure,
        "Couldn't pair from that link.\nphux: error: --code: could not enroll"
    );
    let old = pairing_failure("phux: error: unexpected argument '--code' found");
    assert!(old.contains("phux update"), "{old}");
}
