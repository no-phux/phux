//! Native entrypoints retain the original HELLO scope and publication source.

use super::*;
use phux_client_runtime::Client;
use phux_client_runtime::engine::{EngineError, Scroll};

fn reconnect(client: &Client, server: [u8; 16], bytes: &[u8]) {
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client.feed(phux_wire_fixtures::HelloOk::new(server, phux_wire_fixtures::l3_caps()).build()).unwrap();
    client.with_control(|plane| {
        let attach_id = plane.queued_attach_id();
        // The replacement deliberately reuses resource, stream and bootstrap.
        attach_with_history(plane, attach_id, bytes, None);
    });
    assert_eq!(client.status(), Status::Attached);
    let _ = client.take_outbound();
}

#[test]
fn client_reset_with_identical_wire_ids_rejects_original_source_without_wire_interest() {
    let client = embedded_with_history(None);
    let original = client.view_source(&terminal(), Some(&[0xAB; 16]), client.connection_epoch()).unwrap();
    let original_view = client.create_view(&original).unwrap();
    let old_slot = client.view_slot(original_view).unwrap();
    let old_default = client.slot(&terminal()).unwrap();
    let held = old_slot.acquire().unwrap();
    reconnect(&client, [0xAB; 16], b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive");
    let fresh = client.view_source(&terminal(), Some(&[0xAB; 16]), client.connection_epoch()).unwrap();
    let left = client.create_view(&fresh).unwrap();
    let right = client.clone().create_view(&fresh).unwrap();
    client.scroll_view(left, Scroll::Top).unwrap();
    let default = client.acquire(&terminal()).unwrap();
    let left_frame = client.acquire_view(left).unwrap();
    let right_frame = client.acquire_view(right).unwrap();
    assert_eq!((default.stream_id, default.bootstrap_id), (1, 1));
    assert!(client.take_outbound().is_empty());

    assert!(matches!(client.create_view(&original), Err(EngineError::StaleViewSource)));
    assert!(client.take_outbound().is_empty(), "rejected intent cannot subscribe, attach, resize or spawn");
    assert!(std::sync::Arc::ptr_eq(&default, &client.acquire(&terminal()).unwrap()));
    assert!(std::sync::Arc::ptr_eq(&left_frame, &client.acquire_view(left).unwrap()));
    assert!(std::sync::Arc::ptr_eq(&right_frame, &client.acquire_view(right).unwrap()));
    assert_eq!(left_frame.row_text(0), "zero");
    assert!(right_frame.scrollbar.at_tail());
    assert!(old_slot.acquire().is_none());
    assert!(old_default.acquire().is_none());
    assert!(client.destroy_view(original_view).is_err());
    assert_eq!(held.row_text(0), "two");

    let explicit = client.create_view(&fresh).unwrap();
    client.destroy_view(explicit).unwrap();
    client.destroy_view(left).unwrap();
    assert!(client.acquire_view(right).is_some());
    assert!(client.engine().unwrap().input_ready(&terminal()));
    assert!(client.take_outbound().is_empty(), "presentation lifetimes cannot kill or detach execution");
}

#[test]
fn delayed_native_intent_cannot_replace_its_original_expected_server_with_the_new_hello() {
    let client = embedded_with_history(None);
    let original_server = client.server().unwrap().id;
    let original_epoch = client.connection_epoch();
    let original = client.view_source(&terminal(), Some(&original_server), original_epoch).unwrap();
    reconnect(&client, [0xCD; 16], b"replacement");
    let before = client.acquire(&terminal()).unwrap();

    // A callback waiting for its first source must keep the scope it observed,
    // not ask the new connection which server it should now trust.
    assert!(matches!(
        client.view_source(&terminal(), Some(&original_server), original_epoch),
        Err(EngineError::StaleViewSource)
    ));
    assert!(matches!(client.view_source(&terminal(), None, client.connection_epoch()), Err(EngineError::StaleViewSource)));
    assert!(matches!(client.create_view(&original), Err(EngineError::StaleViewSource)));
    assert!(std::sync::Arc::ptr_eq(&before, &client.acquire(&terminal()).unwrap()));
    assert!(client.take_outbound().is_empty(), "stale callbacks cannot install new interests");
    assert!(client.has_projection(&terminal()));

    // A separate explicit intent names the actual replacement incarnation.
    let fresh = client.view_source(&terminal(), Some(&[0xCD; 16]), client.connection_epoch()).unwrap();
    let view = client.create_view(&fresh).unwrap();
    assert_eq!(client.acquire_view(view).unwrap().text(), "replacement");
    client.destroy_view(view).unwrap();
    assert!(client.engine().unwrap().input_ready(&terminal()));
    assert!(client.take_outbound().is_empty());
}

#[test]
fn retained_offline_source_admits_independent_views_without_transport_or_interest() {
    let client = embedded_with_history(None);
    let original_server = client.server().unwrap().id;
    let source = client.view_source(&terminal(), Some(&original_server), client.connection_epoch()).unwrap();
    client.with_control(|control| control.connection_lost(None));
    assert_ne!(client.status(), Status::Attached);
    let _ = client.take_outbound();

    let left = client.create_view(&source).unwrap();
    let right = client.create_view(&source).unwrap();
    client.scroll_view(left, Scroll::Top).unwrap();
    assert_eq!(client.acquire_view(left).unwrap().row_text(0), "zero");
    assert!(client.acquire_view(right).unwrap().scrollbar.at_tail());
    client.destroy_view(left).unwrap();
    assert!(client.acquire_view(right).is_some());
    assert!(client.has_projection(&terminal()));
    assert!(client.take_outbound().is_empty());
}

#[test]
fn first_source_capture_keeps_original_epoch_even_when_reconnect_reports_the_same_server() {
    let client = embedded_with_history(None);
    let original_server = client.server().unwrap().id;
    let original_epoch = client.connection_epoch();
    reconnect(&client, [0xAB; 16], b"same server, new subscription");
    assert_eq!(client.server().unwrap().id, original_server);
    assert_ne!(client.connection_epoch(), original_epoch);
    let before = client.acquire(&terminal()).unwrap();

    assert!(matches!(
        client.view_source(&terminal(), Some(&original_server), original_epoch),
        Err(EngineError::StaleViewSource)
    ));
    assert!(std::sync::Arc::ptr_eq(&before, &client.acquire(&terminal()).unwrap()));
    assert!(client.take_outbound().is_empty());

    let fresh = client.view_source(&terminal(), Some(&original_server), client.connection_epoch()).unwrap();
    let view = client.create_view(&fresh).unwrap();
    assert_eq!(client.acquire_view(view).unwrap().text(), "same server, new subscription");
    client.destroy_view(view).unwrap();
    assert!(client.take_outbound().is_empty());
}
