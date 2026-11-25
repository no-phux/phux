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
    assert_eq!(
        desk.connection.reorder.target(),
        Some(Grab::Session(key(2)))
    );
    assert!(outbound(&client).is_empty(), "hover is not a move");
    desk.drag_at(grab, Phase::Up, [50.0, 160.0], false);
    let (_, operation) = move_request(&client);
    assert!(desk.owns_sidebar_move(operation));
    assert_eq!(desk.connection.reorder.target(), None);
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
            desk.connection.reorder.held.is_none(),
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
    desk.connection
        .reorder
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
    desk.connection
        .topology
        .as_mut()
        .unwrap()
        .sessions
        .retain(|s| s.key != Some(key(1)));
    assert_eq!(desk.drop_target(Grab::Session(key(2)), [50.0, 80.0]), None);
}

#[test]
fn grouping_a_member_retires_its_old_row_drop_rectangle() {
    let (_, mut desk) = connected();
    let topology = desk.connection.topology.as_mut().unwrap();
    let mut pane = topology.panes[0].clone();
    pane.terminal_id = live(9);
    topology.panes.push(pane);
    let mut resource = topology.resources[0].clone();
    resource.id = live(9);
    topology.resources.push(resource);
    desk.connection.tabs.by_session.insert(
        1,
        vec![super::super::Tab {
            id: [9; 16],
            name: "Split".to_owned(),
            resources: vec![live(9), live(7)],
            active: true,
            locked: false,
        }],
    );
    // @7 is now inside @9's row, but its old rectangle still exists.
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
        desk.connection.shown = shown(7);
        desk.drag_at(Grab::Resource(7, key(1)), Phase::Up, [50.0, 160.0], false);
        let (request, operation) = move_request(&client);
        assert_eq!(
            client.selected_session(),
            Some(1),
            "submission does not switch session"
        );
        if navigate_away {
            desk.connection.shown = shown(8);
        }
        accept(&client, &mut desk, request, operation);
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
}

#[test]
fn an_active_tab_dropped_on_a_session_moves_every_member_using_the_batch_contract() {
    let (client, mut desk) = connected();
    let id = [9; 16];
    push_tab(&mut desk, id, "split", vec![live(7), live(8)]);
    desk.connection
        .reorder
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

#[test]
fn a_pending_navigation_does_not_follow_the_previously_shown_moved_resource() {
    for before_submission in [false, true] {
        let (client, mut desk) = connected();
        desk.connection.shown = shown(7);
        if before_submission {
            desk.want(live(8));
        }
        desk.drag_at(Grab::Resource(7, key(1)), Phase::Up, [50.0, 160.0], false);
        let (request, operation) = move_request(&client);
        if !before_submission {
            desk.want(live(8));
        }
        accept(&client, &mut desk, request, operation);
        assert_eq!(
            client.selected_session(),
            Some(1),
            "pending navigation is already a new selection"
        );
        assert_eq!(
            desk.connection.wanted.as_ref().map(|(id, _)| id),
            Some(&live(8))
        );
    }
}

#[test]
fn satellite_or_missing_tab_members_cannot_be_silently_omitted_from_a_tab_drop() {
    for other in [ResourceId::satellite("elsewhere", 8), live(99)] {
        let (client, mut desk) = connected();
        let id = [9; 16];
        push_tab(&mut desk, id, "mixed", vec![live(7), other]);
        desk.drag_at(Grab::Tab(id), Phase::Up, [50.0, 160.0], false);
        assert!(outbound(&client).is_empty());
        assert!(desk.connection.reorder.moves.is_empty());
    }
}

#[test]
fn accepted_resource_move_is_placed_in_an_unselected_destination_arrangement() {
    use phux_client_core::layout::ArrangementDoc;
    let (client, mut desk) = connected();
    desk.connection.shown = shown(8);
    desk.drag_at(Grab::Resource(7, key(1)), Phase::Up, [50.0, 160.0], false);
    let (request, operation) = move_request(&client);
    accept(&client, &mut desk, request, operation);
    assert!(
        !desk.place_sidebar_moves(&client),
        "wait for authoritative destination membership"
    );
    advertise_membership(&client, 2);
    install_arrangement(&client, 2, &ArrangementDoc::single(live(8)), 1);
    outbound(&client);
    assert!(desk.place_sidebar_moves(&client));
    let written = written_arrangement(&client);
    assert_eq!(written.groups.len(), 2);
    assert_eq!(
        written.groups[0].state.tree,
        Some(phux_client_core::layout::LayoutNode::Leaf(live(8)))
    );
    assert_eq!(
        written.groups[1].state.tree,
        Some(phux_client_core::layout::LayoutNode::Leaf(live(7)))
    );
    assert_eq!(
        client.selected_session(),
        Some(1),
        "placement must not select a background destination"
    );
    assert_eq!(desk.connection.shown.as_ref().unwrap().resource, live(8));
    assert!(
        !desk.place_sidebar_moves(&client),
        "do not submit another write before its receipt and replica revision"
    );
}

#[test]
fn moving_a_tab_preserves_its_split_tree_name_and_group_identity() {
    use phux_client_core::layout::{ArrangementDoc, LayoutNode, SplitDir, split_at};
    let (client, mut desk) = connected();
    let mut source = ArrangementDoc::single(live(7));
    source.groups[0].name = "paired".into();
    source.groups[0].state.tree = Some(
        split_at(
            &LayoutNode::Leaf(live(7)),
            &live(7),
            &live(8),
            SplitDir::Horizontal,
            0.4,
        )
        .unwrap(),
    );
    install_arrangement(&client, 1, &source, 1);
    let id = source.groups[0].id;
    push_tab(&mut desk, id, "paired", vec![live(7), live(8)]);
    outbound(&client);
    desk.drag_at(Grab::Tab(id), Phase::Up, [50.0, 160.0], false);
    let (request, operation) = outbound(&client)
        .into_iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: Command::ResourceMoveMany { operation, .. },
            } => Some((request_id, operation)),
            _ => None,
        })
        .unwrap();
    accept(&client, &mut desk, request, operation);
    advertise_membership(&client, 2);
    install_arrangement(&client, 2, &ArrangementDoc::default(), 1);
    outbound(&client);
    assert!(desk.place_sidebar_moves(&client));
    let written = written_arrangement(&client);
    assert_eq!(written.groups.len(), 1);
    assert_eq!(written.groups[0].id, id);
    assert_eq!(written.groups[0].name, "paired");
    assert_eq!(written.groups[0].state.tree, source.groups[0].state.tree);
}

/// Accept move `operation`, the reply to `request`.
fn accept(
    client: &Client,
    desk: &mut Desk,
    request: u32,
    operation: phux_protocol::ids::OperationId,
) {
    reply(client, desk, request, CommandResult::Accepted { operation });
}

/// `@n` on screen, without a view.
fn shown(n: u32) -> Option<crate::desk::Shown> {
    Some(crate::desk::Shown {
        resource: live(n),
        view: None,
    })
}

/// Add an active, unlocked tab `id` of `resources`.
fn push_tab(desk: &mut Desk, id: [u8; 16], name: &str, resources: Vec<ResourceId>) {
    desk.connection.tabs.list.push(crate::desk::Tab {
        id,
        name: name.into(),
        resources,
        active: true,
        locked: false,
    });
}

fn written_arrangement(client: &Client) -> phux_client_core::layout::ArrangementDoc {
    use phux_client_core::layout::wire::{HandleMap, from_protocol};
    let handles = HandleMap::from_pairs([
        (resource_key(7), Some(live(7))),
        (resource_key(8), Some(live(8))),
    ]);
    outbound(client)
        .into_iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                command:
                    Command::ArrangementReplace {
                        arrangement: target,
                        document,
                        ..
                    },
                ..
            } => {
                assert_eq!(target, arrangement(2));
                Some(from_protocol(&document, &handles).0)
            }
            _ => None,
        })
        .expect("accepted membership must result in a real destination arrangement command")
}
