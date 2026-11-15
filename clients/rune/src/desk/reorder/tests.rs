use super::*;
use phux_client_core::org_ops::OrgOutcome;
use phux_client_runtime::control::Event;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode};

mod fixture;
use fixture::*;

#[test]
fn resource_drop_routes_live_identity_to_stable_server_membership() {
    let (client, mut desk) = connected();
    let grab = Grab::Resource(7, key(1));
    desk.drag_at(grab, Phase::Down, [50.0, 80.0], false);
    desk.drag_at(grab, Phase::Move, [50.0, 160.0], false);
    assert_eq!(desk.reorder.target(), Some(Grab::Session(key(2))));
    assert!(outbound(&client).is_empty(), "hover is not a move");
    desk.drag_at(grab, Phase::Up, [50.0, 160.0], false);
    let (_, operation) = move_request(&client);
    assert!(desk.owns_sidebar_move(operation));
    assert_eq!(desk.reorder.target(), None);
    assert_eq!(
        client
            .organization()
            .resource_by_live(&live(7))
            .unwrap()
            .session,
        key(1),
        "no optimistic ownership mutation"
    );
}

#[test]
fn blank_footer_outside_cancelled_and_same_session_drops_never_move() {
    for (point, cancelled) in [
        ([50.0, 80.0], false),   // same session's resource row
        ([50.0, 50.0], false),   // same session's header
        ([50.0, 140.0], false),  // blank between sessions
        ([50.0, 205.0], false),  // blank after final session
        ([50.0, 230.0], false),  // footer
        ([250.0, 160.0], false), // correct y, outside sidebar
        ([-1.0, 160.0], false),
        ([50.0, 160.0], true),
    ] {
        let (client, mut desk) = connected();
        let grab = Grab::Resource(7, key(1));
        desk.drag_at(grab, Phase::Move, [50.0, 160.0], false);
        desk.drag_at(grab, Phase::Up, point, cancelled);
        assert!(outbound(&client).is_empty(), "invalid drop at {point:?}");
        assert!(
            desk.reorder.held.is_none(),
            "release always clears highlight"
        );
    }
}

#[test]
fn hit_testing_uses_full_rectangles_and_clips_rows_to_the_scroll_viewport() {
    let (_, mut desk) = connected();
    let grab = Grab::Resource(7, key(1));
    assert_eq!(
        desk.drop_target(grab, [8.0, 150.0]),
        Some(Grab::Session(key(2)))
    );
    assert_eq!(desk.drop_target(grab, [192.0, 160.0]), None);
    assert_eq!(desk.drop_target(grab, [50.0, 175.0]), None);
    // A scrolled-off resource can retain an un-clipped layout rect over the footer.
    desk.reorder
        .laid_out(Grab::Resource(7, key(1)), [8.0, 200.0, 184.0, 40.0]);
    assert_eq!(
        desk.drop_target(grab, [50.0, 205.0]),
        Some(Grab::Session(key(1)))
    );
    assert_eq!(desk.drop_target(grab, [50.0, 220.0]), None);
    desk.sidebar_open = false;
    assert_eq!(desk.drop_target(grab, [50.0, 160.0]), None);
}

#[test]
fn resource_rows_are_session_targets_and_deleted_sessions_are_not() {
    let (_, mut desk) = connected();
    assert_eq!(
        desk.drop_target(Grab::Session(key(2)), [50.0, 80.0]),
        Some(Grab::Session(key(1)))
    );
    desk.topology
        .as_mut()
        .unwrap()
        .sessions
        .retain(|s| s.key != Some(key(1)));
    assert_eq!(desk.drop_target(Grab::Session(key(2)), [50.0, 80.0]), None);
}

#[test]
fn moving_a_document_uses_its_organization_key_not_a_terminal_only_lookup() {
    let (client, mut desk) = connected();
    desk.drag_at(Grab::Resource(8, key(1)), Phase::Up, [50.0, 160.0], false);
    assert!(outbound(&client).into_iter().any(|f| matches!(f,
        FrameKind::Command { command: Command::ResourceMove { resource, from, to, .. }, .. }
        if resource == resource_key(8) && from == key(1) && to == key(2)
    )));
}

#[test]
fn stale_source_and_disconnected_client_cannot_submit_moves() {
    let (client, mut desk) = connected();
    desk.move_sidebar_resource(7, key(2), key(1));
    assert!(outbound(&client).is_empty());
    client.with_control(|control| control.connection_lost(Some("offline".into())));
    outbound(&client);
    desk.drag_at(Grab::Resource(7, key(1)), Phase::Up, [50.0, 160.0], false);
    assert!(outbound(&client).is_empty());
}

#[test]
fn server_refusal_and_uncertain_outcomes_are_visible_without_retry_or_selection_change() {
    let (client, mut desk) = connected();
    desk.drag_at(Grab::Resource(7, key(1)), Phase::Up, [50.0, 160.0], false);
    let (request, operation) = move_request(&client);
    reply(
        &client,
        &mut desk,
        request,
        CommandResult::Error {
            code: ErrorCode::PermissionDenied,
            message: "move denied".to_owned(),
        },
    );
    assert!(desk.notice.as_ref().unwrap().text.contains("move denied"));
    assert!(!desk.owns_sidebar_move(operation));
    assert_eq!(client.selected_session(), Some(1));
    assert!(outbound(&client).is_empty());
    desk.drag_at(Grab::Resource(7, key(1)), Phase::Up, [50.0, 160.0], false);
    let (_, operation) = move_request(&client);
    assert!(desk.sidebar_move_outcome(
        &client,
        operation,
        &OrgOutcome::Unknown {
            reason: "expired".into()
        }
    ));
    assert!(desk.notice.as_ref().unwrap().text.contains("expired"));
    assert!(outbound(&client).is_empty());
}

#[test]
fn successful_receipt_refreshes_inventory_and_follows_only_the_still_selected_resource() {
    for navigate_away in [false, true] {
        let (client, mut desk) = connected();
        desk.shown = Some(crate::desk::Shown {
            resource: live(7),
            view: None,
        });
        desk.drag_at(Grab::Resource(7, key(1)), Phase::Up, [50.0, 160.0], false);
        let (request, operation) = move_request(&client);
        assert_eq!(
            client.selected_session(),
            Some(1),
            "submission does not switch session"
        );
        if navigate_away {
            desk.shown = Some(crate::desk::Shown {
                resource: live(8),
                view: None,
            });
        }
        reply(
            &client,
            &mut desk,
            request,
            CommandResult::Accepted { operation },
        );
        assert_eq!(
            client.selected_session(),
            Some(if navigate_away { 1 } else { 2 })
        );
        assert!(!desk.owns_sidebar_move(operation));
        let sent = outbound(&client);
        assert!(sent.iter().any(|f| matches!(
            f,
            FrameKind::Command {
                command: Command::GetOrganization { .. },
                ..
            }
        )));
        assert!(!desk.sidebar_move_outcome(&client, operation, &OrgOutcome::Accepted));
    }
}

#[test]
fn session_reorder_keeps_the_whole_stored_order_even_over_a_resource_row() {
    let (client, mut desk) = connected();
    desk.drag_at(Grab::Session(key(2)), Phase::Up, [50.0, 80.0], false);
    assert!(outbound(&client).iter().any(|f| matches!(f,
        FrameKind::SetMetadata { scope: Scope::Global, key: metadata_key, value, .. }
        if metadata_key == SESSION_ORDER_KEY && session_order::decode(Some(value)) == vec![key(2), key(1)]
    )));
    assert_eq!(
        moved(vec![key(1), key(2)], key(2), key(1)),
        Some(vec![key(2), key(1)])
    );
}

#[test]
fn an_active_tab_dropped_on_a_session_moves_every_member_using_the_batch_contract() {
    let (client, mut desk) = connected();
    let id = [9; 16];
    desk.tabs.list.push(crate::desk::Tab {
        id,
        name: "split".into(),
        resources: vec![live(7), live(8)],
        active: true,
        locked: false,
    });
    desk.reorder
        .laid_out(Grab::Tab(id), [210.0, 0.0, 100.0, 30.0]);
    assert_eq!(
        desk.drop_target(Grab::Tab(id), [250.0, 15.0]),
        Some(Grab::Tab(id))
    );
    assert_eq!(
        desk.drop_target(Grab::Tab(id), [250.0, 35.0]),
        None,
        "below tab strip is not a tab reorder"
    );
    desk.drag_at(Grab::Tab(id), Phase::Up, [50.0, 160.0], false);
    assert!(outbound(&client).into_iter().any(|f| matches!(f,
        FrameKind::Command { command: Command::ResourceMoveMany { items, to, .. }, .. }
        if to == key(2) && items.iter().map(|item| item.resource).collect::<Vec<_>>() == vec![resource_key(7), resource_key(8)]
    )));
}

#[test]
fn unverified_server_identity_retires_pending_resource_moves() {
    let (client, mut desk) = connected();
    desk.drag_at(Grab::Resource(7, key(1)), Phase::Up, [50.0, 160.0], false);
    let (_, operation) = move_request(&client);
    assert!(desk.cancel_sidebar_moves());
    assert!(!desk.owns_sidebar_move(operation));
    assert!(!desk.sidebar_move_outcome(&client, operation, &OrgOutcome::Accepted));
    assert!(outbound(&client).is_empty());
}
