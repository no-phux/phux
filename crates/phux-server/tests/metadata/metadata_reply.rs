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
                &FrameKind::SetMetadata {
                    request_id,
                    scope: scope.clone(),
                    key: key.to_owned(),
                    value: value.to_vec(),
                },
            )
            .await;
        }
        for (request_id, key, expected) in [
            (0xCAFE_F00D, LAYOUT_KEY, Some(LAYOUT_VALUE)),
            (0xDEAD_BEEF, "phux.never.set/v1", None),
        ] {
            send_frame(
                &mut stream,
                &FrameKind::GetMetadata {
                    request_id,
                    scope: scope.clone(),
                    key: key.to_owned(),
                },
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
