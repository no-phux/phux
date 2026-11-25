//! Real focus/open entrypoints must retain the publication and first-source scope.

use super::*;
use crate::test_support::{self, terminal::Replica};

fn id(n: u32) -> ResourceId { ResourceId::local(n) }

#[test]
fn a_captured_focus_source_cannot_rebind_identical_wire_ids_on_another_server() {
    let replica = Replica::new(80, 24, 2);
    let client = &replica.client;
    let mut desk = replica.window();
    desk.want(id(2)); // Captures before the deferred open.
    replica.replace_server(2); // Reuses resource 2, stream 2 and bootstrap 2.
    let mut rt = test_support::runtime(desk, crate::ui::view, test_support::SIZE, rune::Env::new(true, 1.0));
    rt.update(|desk, tx| {
        desk.observe(client, client.take_activity(), tx);
        assert!(!desk.open_wanted(client));
        assert!(desk.connection.shown.is_none());
        assert!(desk.connection.wanted.is_none());
        // A qualified new gesture, unlike the old continuation, may open it.
        desk.show(id(2), "budget");
        assert_eq!(desk.connection.shown.as_ref().map(|shown| &shown.resource), Some(&id(2)));
    });
}

#[test]
fn waiting_for_a_first_source_does_not_upgrade_to_a_new_epoch_even_on_the_same_server() {
    for server in [1, 2] {
        let replica = Replica::pending(80, 24, 2);
        let client = &replica.client;
        let mut desk = replica.window();
        desk.show(id(2), "budget");
        assert!(desk.connection.wanted.is_some());
        replica.replace_server(server);
        let _ = client.take_outbound();
        assert!(!desk.open_wanted(client));
        assert!(desk.connection.shown.is_none());
        assert!(desk.connection.wanted.is_none());
        assert!(client.take_outbound().is_empty(), "no stale subscription or session attach");
    }
}

#[test]
fn an_unrendered_focus_intent_is_bound_to_its_original_runtime_allocation() {
    let original = Replica::pending(80, 24, 2);
    let replacement = Replica::new(80, 24, 2); // Same server, epoch and wire ids.
    let mut desk = original.window();
    desk.show(id(2), "budget");
    // Exercise admission with an old intent and a replaced embedded client.
    desk.connection.client = Some(replacement.client.clone());
    assert!(!desk.open_wanted(&replacement.client));
    assert!(desk.connection.shown.is_none());
    assert!(desk.connection.wanted.is_none());
    assert!(replacement.client.take_outbound().is_empty());
}

#[test]
fn the_latest_focus_choice_wins_over_an_older_pending_open_and_spawn_receipt() {
    let replica = Replica::new(80, 24, 2);
    let client = &replica.client;
    let mut desk = replica.window();
    desk.new_terminal();
    let request = desk.connection.pending_spawns[0];
    desk.want(id(2));
    desk.want(id(1));
    assert!(desk.open_wanted(client));
    client.feed(phux_protocol::wire::frame::FrameKind::ResourceSpawned {
        request_id: request,
        result: phux_protocol::wire::frame::SpawnResult::Ok(id(2)),
    }).unwrap();
    let mut rt = test_support::runtime(desk, crate::ui::view, test_support::SIZE, rune::Env::new(true, 1.0));
    rt.update(|desk, tx| {
        desk.observe(client, client.take_activity(), tx);
        desk.open_wanted(client);
    });
    assert_eq!(rt.state.connection.shown.as_ref().map(|shown| &shown.resource), Some(&id(1)));
    assert!(rt.state.connection.pending_spawns.is_empty());
}

#[test]
fn a_retained_offline_publication_can_open_and_close_an_independent_view_without_ending_execution() {
    let replica = Replica::new(80, 24, 1);
    let client = &replica.client;
    let mut desk = replica.window();
    desk.want(id(1));
    client.with_control(|control| control.connection_lost(None));
    assert!(desk.open_wanted(client));
    let view = desk.connection.shown.as_ref().unwrap().view.unwrap();
    assert_ne!(view, replica.views[0]);
    let _ = client.take_outbound();
    desk.drop_shown(client);
    assert!(client.view_generation(view).is_none());
    assert!(client.acquire_view(replica.views[0]).is_some(), "the sibling view and execution replica survive");
    assert!(client.take_outbound().is_empty(), "view close is neither terminate nor detach");
}

#[test]
fn runtime_replacement_drops_numeric_intent_and_rejects_old_owner_observations() {
    let original = Replica::pending(80, 24, 2);
    let replacement = Replica::new(80, 24, 2);
    let mut desk = original.window();
    desk.show(id(2), "budget");
    let observation = original.client.take_activity();
    desk.replace_connection(crate::desk::link::Replacement::Retry);
    desk.connection.client = Some(replacement.client.clone());
    let mut rt = test_support::runtime(desk, crate::ui::view, test_support::SIZE, rune::Env::new(true, 1.0));
    rt.update(|desk, tx| {
        assert_eq!(desk.observe(&original.client, observation, tx), (false, false));
    });
    assert!(rt.state.connection.wanted.is_none());
    assert!(rt.state.connection.shown.is_none());
}

#[test]
fn a_pending_session_name_cannot_select_a_same_named_session_on_a_replacement_epoch() {
    let replica = Replica::new(80, 24, 1);
    let client = &replica.client;
    let mut desk = replica.window();
    desk.wait_for_session("budget".to_owned());
    replica.replace_server(2);
    let _ = client.take_outbound();
    assert!(!desk.open_wanted(client));
    assert!(desk.connection.pending_session.is_none());
    assert!(desk.connection.shown.is_none());
    assert!(client.take_outbound().is_empty());
}

#[test]
fn connecting_with_cached_hello_does_not_requalify_the_old_topology_scope() {
    let replica = Replica::pending(80, 24, 2);
    let client = &replica.client;
    let desk = replica.window();
    client.with_control(phux_client_runtime::control::ControlPlane::connection_opened);
    let mut rt = test_support::runtime(desk, crate::ui::view, test_support::SIZE, rune::Env::new(true, 1.0));
    rt.update(|desk, tx| {
        desk.observe(client, client.take_activity(), tx);
        assert_eq!(desk.connection.epoch, 2);
        assert_eq!(desk.connection.view_epoch, 1, "cached HELLO is not a new qualified source scope");
        desk.show(id(2), "budget");
        assert!(desk.connection.shown.is_none());
        assert!(desk.connection.wanted.is_none());
    });
}

#[test]
fn negotiated_replacement_does_not_relabel_cached_inventory_before_fresh_attach() {
    let replica = Replica::new(80, 24, 2);
    let client = &replica.client;
    let desk = replica.window();
    client.with_control(phux_client_runtime::control::ControlPlane::connection_opened);
    client.feed(phux_wire_fixtures::HelloOk::new(
        vec![2; 16], phux_protocol::caps::ServerCapabilities::new(),
    ).build()).unwrap();
    assert_eq!(client.status(), Status::Negotiated);
    let _ = client.take_outbound();
    let mut rt = test_support::runtime(desk, crate::ui::view, test_support::SIZE, rune::Env::new(true, 1.0));
    rt.update(|desk, tx| {
        desk.observe(client, client.take_activity(), tx);
        assert_eq!(desk.connection.view_server.as_deref(), Some(&[1_u8; 16][..]));
        assert_eq!(desk.connection.view_epoch, 1);
        desk.show(id(2), "budget");
        assert!(desk.connection.wanted.is_none());
        assert!(desk.connection.shown.is_none());
    });
    assert!(client.take_outbound().is_empty(), "an old row cannot install replacement interest");
}
