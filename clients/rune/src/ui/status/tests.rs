//! The window's words about itself, on a desktop and on the phone app.

use super::*;
use phux_protocol::ids::ResourceId;

#[test]
fn settings_open_from_the_titlebar_with_or_without_the_sidebar() {
    use rune::accesskit::Action;

    for sidebar_open in [true, false] {
        let mut desk = Desk::settings_test_fixture();
        desk.settings_open = false;
        desk.sidebar_open = sidebar_open;
        let mut rt = rune::Runtime::new(
            desk,
            crate::ui::view,
            (1100.0, 720.0),
            rune::Env::new(false, 1.0),
        );
        rt.frame(0.0);
        assert!(rt.find("settings-button").is_some());
        assert!(rt.accessibility_action("settings-button", Action::Click, None));
        rt.frame(0.0);
        assert!(rt.state.settings_open);
        assert!(rt.find("settings-about").is_some());
    }
}

#[test]
fn a_remote_servers_update_happens_on_that_machine() {
    let behind = Standing::Incompatible("server requires 12.x; update the phux server".to_owned());
    assert!(updates_can_fix(&behind, false));
    assert!(!updates_can_fix(&behind, true));
    assert!(
        words(&behind, "mini", true)
            .1
            .contains("`phux update` on mini")
    );
    assert!(!words(&behind, "here", false).1.contains("`phux update`"));
    // The app is this computer's to update, wherever the server is.
    let ahead = Standing::Incompatible("update the phux app/client".to_owned());
    assert!(updates_can_fix(&ahead, true));
}

#[test]
fn a_held_tab_close_counts_its_terminals() {
    let close = |n| Pending::CloseTab((0..n).map(ResourceId::local).collect());
    assert_eq!(
        question(&close(1), false).0,
        "Close this tab? Anything running in it ends."
    );
    assert_eq!(
        question(&close(3), false).0,
        "Close this tab and its 3 terminals? Anything running in them ends."
    );
}

/// The phone app, paired with a computer called `mini`.
fn phone() -> Desk {
    let configuration = serde_json::json!({"endpoint": "quic://localhost:8788", "name": "mini"});
    crate::mobile::state(serde_json::from_value(configuration).expect("a configuration"))
        .expect("a loopback host needs no pairing")
}

#[test]
fn a_touch_confirmation_names_no_key() {
    let paste = Pending::Paste(ResourceId::local(1), "a\nb".to_owned());
    let session = Pending::CloseSession(
        phux_protocol::ids::SessionKey::new([1; 16]).expect("a key"),
        "work".to_owned(),
    );
    let asks = [
        (paste, "Paste"),
        (Pending::Close(ResourceId::local(1)), "Close"),
        (Pending::CloseTab(vec![ResourceId::local(1)]), "Close"),
        (
            Pending::CloseTab(vec![ResourceId::local(1), ResourceId::local(2)]),
            "Close",
        ),
        (session, "End session"),
    ];
    for (pending, verb) in asks {
        assert_eq!(question(&pending, true).1, verb);
        assert_eq!(question(&pending, false).1, format!("{verb} (Return)"));
        assert_eq!(question(&pending, true).0, question(&pending, false).0);
    }
}

#[test]
fn the_phone_app_speaks_of_its_computer_and_of_no_command() {
    let mut desk = phone();
    let hint = "Check that Tailscale is on and mini is awake.".to_owned();
    assert_eq!(
        said(&desk, &Standing::Unreachable(hint.clone())),
        ("Can't reach mini".to_owned(), hint.clone())
    );
    assert_eq!(
        said(&desk, &Standing::Connecting(String::new())),
        ("Connecting to mini…".to_owned(), String::new())
    );
    let behind = Standing::Incompatible("server requires 12.x; update the phux server".to_owned());
    let (title, detail) = said(&desk, &behind);
    assert_eq!(title, "Update phux on mini");
    assert!(!detail.contains('`'), "{detail}");
    // What a desktop says the same way stays as it is.
    assert_eq!(said(&desk, &Standing::NoSessions).0, "No sessions");
    // Refused: the title says why nothing will retry.
    desk.error = Some("mini refused the pairing token (unauthorized)".to_owned());
    let refused = desk.standing();
    assert!(matches!(&refused, Standing::Unreachable(why) if why.ends_with("Pair again.")));
    assert_eq!(said(&desk, &refused).0, "No longer paired");
    assert!(phone::repairs(&desk, &refused));
    assert!(!phone::repairs(&desk, &Standing::Closed(String::new())));
    // A desktop keeps its own words for all of it.
    let desktop = Desk::settings_test_fixture();
    assert_eq!(said(&desktop, &refused).0, "Can't reach phux");
    assert!(!phone::repairs(&desktop, &refused));
}
