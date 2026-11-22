//! L3 `GET_METADATA` / `LIST_METADATA` replies over the wire: a set value
//! round-trips with its `request_id` echoed, an absent key answers `None`, and
//! keys list sorted. No ATTACH: the L3 arms work on the connection itself.

use phux_protocol::ids::GroupId;
use phux_protocol::wire::frame::{FrameKind, Scope};

use phux_server_testkit::{recv_typed, run_local, send_frame, spawn_server_connected};

const LAYOUT_KEY: &str = "phux.tui.layout/v1";
const OTHER_KEY: &str = "phux.tui.window_order/v1";
const LAYOUT_VALUE: &[u8] = b"\xa2\x01\x01\x02\x82\x00\x01";

#[test]
fn metadata_get_and_list_replies_round_trip() {
    run_local(async {
        let (_server, mut stream) = spawn_server_connected(None).await;
        let scope = Scope::Group(GroupId::new(1));
        // Deliberately unsorted; SET has no reply, and frames are handled in order.
        for (request_id, key, value) in [(1, OTHER_KEY, &b"o"[..]), (2, LAYOUT_KEY, LAYOUT_VALUE)] {
            send_frame(
                &mut stream,
                &phux_server_testkit::set_metadata(request_id, scope.clone(), key, value.to_vec()),
            )
            .await;
        }
        for (request_id, key, expected) in [
            (0xCAFE_F00D, LAYOUT_KEY, Some(LAYOUT_VALUE)),
            (0xDEAD_BEEF, "phux.never.set/v1", None),
        ] {
            send_frame(
                &mut stream,
                &phux_server_testkit::get_metadata(request_id, scope.clone(), key),
            )
            .await;
            let (_, reply) = recv_typed(&mut stream).await;
            let FrameKind::MetadataValue {
                request_id: got,
                value,
            } = reply
            else {
                panic!("expected METADATA_VALUE, got {reply:?}");
            };
            assert_eq!(got, request_id, "request_id echoes verbatim");
            assert_eq!(value.as_deref(), expected, "{key}");
        }

        send_frame(
            &mut stream,
            &FrameKind::ListMetadata {
                request_id: 0x99,
                scope,
            },
        )
        .await;
        let (_, reply) = recv_typed(&mut stream).await;
        let FrameKind::MetadataKeys { request_id, keys } = reply else {
            panic!("expected METADATA_KEYS, got {reply:?}");
        };
        assert_eq!(request_id, 0x99);
        assert_eq!(keys, [LAYOUT_KEY, OTHER_KEY], "sorted");
    });
}

#[test]
fn legacy_layout_metadata_write_commits_the_typed_arrangement() {
    use phux_client_core::layout::{ArrangementDoc, Group, LayoutNode, LayoutState};
    use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, OrgFilter};
    use phux_server_testkit::{SpawnedServer, attach_focused, command, set_metadata, sh};

    run_local(async {
        let server = SpawnedServer::seeded("work", sh("read _line"));
        let mut reader = server.connect().await;
        let pane = attach_focused(&mut reader, "work").await;
        let query = Command::GetOrganization {
            filter: OrgFilter::All,
            page_size: 0,
            cursor: None,
        };
        let CommandResult::OkWith(CommandValue::OrganizationPage(page)) =
            command(&mut reader, 1, query.clone()).await
        else {
            panic!("organization snapshot unavailable");
        };
        let session = page
            .sessions
            .iter()
            .find(|session| session.name == "work")
            .unwrap();
        let key = phux_protocol::wire::info::tui_layout_key(session.live.unwrap());
        let group_id = [7; 16];
        let legacy = ArrangementDoc {
            groups: vec![Group::with_id(
                group_id,
                "from old peer".to_owned(),
                LayoutState {
                    tree: Some(LayoutNode::Leaf(pane.clone())),
                    focus: Some(pane),
                },
            )],
            active: 0,
            slots: Vec::new(),
        };
        let scope = Scope::Group(GroupId::new(1));
        // Both frames use the same connection. The query is a barrier for
        // the legacy write, whose wire contract has no reply.
        send_frame(
            &mut reader,
            &set_metadata(2, scope, &key, legacy.encode_cbor().unwrap()),
        )
        .await;
        let CommandResult::OkWith(CommandValue::OrganizationPage(page)) =
            command(&mut reader, 3, query).await
        else {
            panic!("organization snapshot unavailable");
        };
        let session = page
            .sessions
            .iter()
            .find(|session| session.name == "work")
            .unwrap();
        let query = Command::ArrangementGet {
            arrangement: session.arrangement.unwrap(),
        };
        let CommandResult::OkWith(CommandValue::Arrangement(value)) =
            command(&mut reader, 4, query).await
        else {
            panic!("translated arrangement unavailable");
        };
        assert_eq!(value.document.groups.len(), 1);
        assert_eq!(value.document.groups[0].id, group_id);
        assert_eq!(
            value.document.groups[0].name.as_deref(),
            Some("from old peer")
        );
        drop(reader);
        server.stop().await;
    });
}
