//! A picked file goes to the pane it was asked for, or nowhere and says why.

use super::*;
use crate::desk::Shown;
use crate::test_support::{
    notice,
    terminal::{DURABLE_AUTHORITY, Replica, organized},
};
use phux_client_runtime::control::{ControlPlane, Status};
use phux_protocol::caps::{ServerCapabilities, ServerFeature, ServerFeatureSet};
use phux_protocol::ids::{AuthorityId, Revision, SessionKey};
use phux_protocol::wire::frame::{CommandValue, OrganizationPage, ResourceRecord};

fn replica() -> Replica {
    let capabilities = ServerCapabilities::new()
        .with_features(ServerFeatureSet::with(&[ServerFeature::FileUpload]));
    Replica::with_capabilities(80, 24, 2, organized(capabilities))
}

fn phone_with(replica: &Replica) -> Desk {
    let mut desk = replica.window().with_phone(true);
    desk.native = Some(crate::mobile::Platform::test());
    show(&mut desk, 1);
    desk
}

fn phone() -> Desk {
    phone_with(&replica())
}

fn seed_key(replica: &Replica) {
    let request = replica
        .client
        .with_control(ControlPlane::refresh_organization)
        .unwrap();
    let record = ResourceRecord::new(
        ResourceKey::new([1; 16]).unwrap(),
        SessionKey::new([1; 16]).unwrap(),
        Revision::new(1),
    )
    .with_live(Some(ResourceId::local(1)));
    let page = OrganizationPage::new(
        AuthorityId::new(DURABLE_AUTHORITY).unwrap(),
        Revision::new(1),
        0,
    )
    .with_resources(vec![record]);
    replica
        .client
        .feed(phux_wire_fixtures::command_value(
            request,
            CommandValue::OrganizationPage(Box::new(page)),
        ))
        .unwrap();
}

fn show(desk: &mut Desk, terminal: u32) {
    desk.connection.shown = Some(Shown {
        resource: ResourceId::local(terminal),
        view: phux_client_runtime::ViewId::from_raw(1),
    });
}

fn ask(desk: &mut Desk) -> u64 {
    desk.screenshot();
    let native = desk.native.as_mut().unwrap();
    assert!(
        std::mem::take(&mut native.attach_requested),
        "asks the host"
    );
    native.attaching.front().unwrap().id
}

fn targets(desk: &Desk) -> Vec<ResourceId> {
    let uploads = desk.connection.uploads.iter();
    uploads.map(|(_, terminal, _)| terminal.clone()).collect()
}

#[test]
fn every_pick_goes_to_the_pane_it_was_asked_for() {
    let mut desk = phone();
    let first = ask(&mut desk);
    show(&mut desk, 2);
    let second = ask(&mut desk);
    desk.attach_from_host(second, "pdf", Some(vec![1]));
    // The first photo was still loading, and there were two of them.
    desk.attach_from_host(first, "jpg", Some(vec![1]));
    desk.attach_from_host(first, "png", Some(vec![1]));
    let panes = [2, 1, 1].map(ResourceId::local);
    assert_eq!(targets(&desk), panes);
    assert_eq!(notice(&desk), "Uploading image…");
}

#[test]
fn what_cannot_be_sent_says_why_and_uploads_nothing() {
    let mut desk = phone();
    let id = ask(&mut desk);
    desk.attach_from_host(id, "jpg", None);
    assert!(notice(&desk).contains("couldn't be read"));
    desk.attach_from_host(id + 1, "jpg", Some(vec![1]));
    assert!(notice(&desk).contains("too much"));
    desk.left_server();
    desk.attach_from_host(id, "jpg", Some(vec![1]));
    assert!(notice(&desk).contains("another computer"));
    assert!(targets(&desk).is_empty());
}

#[test]
fn a_question_with_the_keyboard_holds_attachments_off() {
    let mut desk = phone();
    let id = ask(&mut desk);
    desk.settings_open = true;
    desk.attach_from_host(id, "jpg", Some(vec![1]));
    assert!(targets(&desk).is_empty());
    desk.screenshot();
    assert!(!desk.native.as_ref().unwrap().attach_requested);
}

#[test]
fn without_a_terminal_nothing_is_picked() {
    let mut desk = phone();
    desk.connection.shown = None;
    desk.screenshot();
    assert!(!desk.native.as_ref().unwrap().attach_requested);
    assert_eq!(notice(&desk), "No terminal to attach to.");
}

#[test]
fn a_delayed_pick_cannot_cross_a_replaced_runtime_even_with_the_same_pane_id() {
    use crate::desk::link::Replacement;

    for policy in [Replacement::Retry, Replacement::Machine] {
        let mut desk = phone();
        let id = ask(&mut desk);
        desk.replace_connection(policy);
        // The replacement can be connected before its topology arrives. Even
        // reuse of the old pane number cannot revive a picker.
        let replacement = replica();
        desk.connection.client = Some(replacement.client.clone());
        show(&mut desk, 1);
        assert!(desk.connection.topology.is_none());
        desk.attach_from_host(id, "jpg", Some(vec![1]));
        assert!(
            targets(&desk).is_empty(),
            "old request uploaded after replacement"
        );
        desk.connection
            .seed_view_sources(vec![1; 16], &replacement.sources);
        let next = ask(&mut desk);
        assert!(next > id, "request IDs cannot regain an old callback");
        desk.attach_from_host(id, "png", Some(vec![1]));
        assert!(targets(&desk).is_empty());
        desk.attach_from_host(next, "png", Some(vec![1]));
        assert_eq!(targets(&desk), [ResourceId::local(1)]);
    }
}

#[test]
fn a_pick_delivered_during_transport_recovery_is_not_queued_for_the_next_server() {
    for opened in [false, true] {
        let replica = replica();
        seed_key(&replica);
        let mut desk = phone_with(&replica);
        let id = ask(&mut desk);
        assert!(
            desk.native
                .as_ref()
                .unwrap()
                .attaching
                .front()
                .unwrap()
                .key
                .is_some()
        );
        replica.client.with_control(|control| {
            control.connection_lost(None);
            if opened {
                control.connection_opened();
            }
            assert_eq!(control.status(), Status::Connecting);
            assert_eq!(control.server().unwrap().id, vec![1; 16]);
        });
        assert!(
            desk.connection.topology.is_some(),
            "old topology is retained"
        );
        desk.attach_from_host(id, "jpg", Some(vec![1]));
        assert!(
            targets(&desk).is_empty(),
            "unverified bytes must not enter replay"
        );
    }
}

#[test]
fn a_pick_survives_same_server_recovery_but_not_a_new_incarnation() {
    let replica = replica();
    let mut desk = phone_with(&replica);
    let id = ask(&mut desk);
    let epoch = replica
        .client
        .with_control(|control| control.connection_epoch());
    replica.replace_server(1);
    assert!(
        replica
            .client
            .with_control(|control| control.connection_epoch())
            > epoch
    );
    desk.attach_from_host(id, "jpg", Some(vec![1]));
    assert_eq!(targets(&desk), [ResourceId::local(1)]);
    desk.connection.uploads.clear();
    // Retain the original request and the Desk's old cached topology. This
    // server has not seeded a resource key to save us.
    assert!(
        desk.native
            .as_ref()
            .unwrap()
            .attaching
            .front()
            .unwrap()
            .key
            .is_none()
    );
    replica.replace_server(2);
    assert_eq!(replica.client.status(), Status::Attached);
    desk.attach_from_host(id, "png", Some(vec![1]));
    assert!(
        targets(&desk).is_empty(),
        "recycled wire ID is not the old pane"
    );
}

fn refused_native_request(older: bool, recovered: bool, paste: bool) {
    let replica = replica();
    let mut desk = phone_with(&replica);
    let previous = older.then(|| ask(&mut desk));
    show(&mut desk, 2);
    replica
        .client
        .with_control(|control| control.connection_lost(None));
    if recovered {
        replica.replace_server(1);
    }
    let mut rt = refused_native_action(desk, paste);
    if let Some(id) = previous {
        if !recovered {
            replica.replace_server(1);
        }
        // The actual late answer still belongs to pane 1; do not
        // clear legitimate picker requests to suppress this paste.
        rt.state.attach_from_host(id, "jpg", Some(vec![1]));
        assert_eq!(targets(&rt.state), [ResourceId::local(1)]);
    }
}

fn refused_native_action(mut desk: Desk, paste: bool) -> rune::Runtime<Desk> {
    // Also cancel this kind of unconsumed request flag on failure.
    let native = desk.native.as_mut().unwrap();
    native.paste_requested = true;
    native.attach_requested = true;
    let mut rt = crate::test_support::runtime(
        desk,
        crate::ui::view,
        (393.0, 852.0),
        rune::Env::new(true, 1.0),
    );
    rt.update(|desk, tx| {
        if paste {
            desk.shortcut(crate::keys::Shortcut::Paste, tx);
        } else {
            desk.screenshot();
        }
    });
    let requests = crate::mobile::take_requests(&mut rt);
    assert_eq!(
        requests["paste"], false,
        "failed capture must not read a new clipboard"
    );
    assert!(
        requests["attaching"].is_null(),
        "never export an older request for new bytes"
    );
    assert!(
        requests["attach"].is_null(),
        "failed capture must not open a picker"
    );
    rt
}

#[test]
fn a_refused_native_paste_cannot_borrow_an_older_pickers_request() {
    for older in [false, true] {
        for recovered in [false, true] {
            refused_native_request(older, recovered, true);
        }
    }
}

#[test]
fn a_refused_native_picker_cannot_export_an_older_request_flag() {
    for older in [false, true] {
        for recovered in [false, true] {
            refused_native_request(older, recovered, false);
        }
    }
}

#[test]
fn early_native_refusals_drop_flags_without_retiring_legitimate_pickers() {
    for paste in [false, true] {
        for reason in ["missing", "viewless", "modal"] {
            let mut desk = phone();
            let id = ask(&mut desk);
            match reason {
                "missing" => desk.connection.shown = None,
                "viewless" => desk.connection.shown.as_mut().unwrap().view = None,
                _ => desk.settings_open = true,
            }
            let mut rt = refused_native_action(desk, paste);
            rt.state.settings_open = false;
            show(&mut rt.state, 2);
            rt.state.attach_from_host(id, "jpg", Some(vec![1]));
            assert_eq!(targets(&rt.state), [ResourceId::local(1)]);
        }
    }
}

#[test]
fn a_successful_native_paste_exports_its_own_pane_bound_request() {
    let mut desk = phone();
    let previous = ask(&mut desk);
    show(&mut desk, 2);
    let mut rt = crate::test_support::runtime(
        desk,
        crate::ui::view,
        (393.0, 852.0),
        rune::Env::new(true, 1.0),
    );
    rt.update(|desk, tx| desk.shortcut(crate::keys::Shortcut::Paste, tx));
    let requests = crate::mobile::take_requests(&mut rt);
    assert_eq!(requests["paste"], true);
    let id = requests["attaching"].as_u64().unwrap();
    assert!(id > previous);
    crate::mobile::attach(&mut rt, id, "jpg", Some(vec![1]));
    crate::mobile::attach(&mut rt, previous, "jpg", Some(vec![1]));
    assert_eq!(targets(&rt.state), [2, 1].map(ResourceId::local));
}

#[test]
fn cross_kind_native_requests_never_relabel_an_undrained_intent() {
    for paste in [false, true] {
        let mut desk = phone();
        let previous = ask(&mut desk);
        show(&mut desk, 2);
        let native = desk.native.as_mut().unwrap();
        native.paste_requested = !paste;
        native.attach_requested = paste;
        let mut rt = crate::test_support::runtime(
            desk,
            crate::ui::view,
            (393.0, 852.0),
            rune::Env::new(true, 1.0),
        );
        rt.update(|desk, tx| {
            if paste {
                desk.shortcut(crate::keys::Shortcut::Paste, tx);
            } else {
                desk.screenshot();
            }
        });
        let requests = crate::mobile::take_requests(&mut rt);
        assert_eq!(
            requests["paste"], paste,
            "the latest intent owns the one exported ID"
        );
        assert_eq!(requests["attach"].is_array(), !paste);
        let id = requests["attaching"].as_u64().unwrap();
        assert!(id > previous);
        crate::mobile::attach(&mut rt, id, "jpg", Some(vec![1]));
        crate::mobile::attach(&mut rt, previous, "jpg", Some(vec![1]));
        assert_eq!(targets(&rt.state), [2, 1].map(ResourceId::local));
    }
}
