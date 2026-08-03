//! `phux-4li.8` — L3 metadata `GET_METADATA` / `LIST_METADATA` wire reply
//! path.
//!
//! Drives a freshly-spawned `ServerRuntime` over a real Unix-domain socket,
//! seeds an L3 metadata key via `SET_METADATA`, and verifies that:
//!
//! 1. `GET_METADATA` for that key elicits a `METADATA_VALUE { request_id,
//!    value: Some(bytes) }` reply over the wire, with `request_id` echoed
//!    verbatim and `value` equal to the bytes we set.
//! 2. `GET_METADATA` for a non-existent key elicits `METADATA_VALUE
//!    { request_id, value: None }`.
//! 3. `LIST_METADATA` elicits `METADATA_KEYS { request_id, keys }` with
//!    the keys present in the scope (lexicographically sorted).
//!
//! Together these close the GET-reply gap that `phux-4li.2` deferred —
//! a client can now fetch the current value of any L3 key over the wire
//! at attach time, unblocking the reattach-restores-layout path
//! (one of the phux-4li epic's success criteria) and `phux-4li.5`'s
//! reconcile-on-attach use case.
//!
//! The connection here does not ATTACH and does not send HELLO: the L3
//! dispatch arms operate on the connection's `client_id`, not on its
//! attached session, and `ServerState::client_layers` defaults to
//! `LayerSet::all()` for an un-HELLO'd client (so L3 is permitted by
//! default — the SPEC §16.4 gate fires only against an explicitly
//! L3-less HELLO, which is its own test in `phux-protocol`).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use phux_protocol::ids::GroupId;
use phux_protocol::wire::frame::{FrameKind, Scope, TYPE_METADATA_KEYS, TYPE_METADATA_VALUE};
use tempfile::TempDir;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, recv_typed, run_local, send_frame, spawn_server, wait_for_socket,
};

const LAYOUT_KEY: &str = "phux.tui.layout/v1";
const OTHER_KEY: &str = "phux.tui.window_order/v1";
const LAYOUT_VALUE: &[u8] = b"\xa2\x01\x01\x02\x82\x00\x01"; // CBOR-ish fixture

const fn collection_scope() -> Scope {
    // Matches `phux_server::state::DEFAULT_GROUP_ID`. The wire-side
    // GroupId is a bare u32 in v0.1 (see phux-4li.2's commit body).
    Scope::Group(GroupId::new(1))
}

#[test]
fn metadata_get_reply_present_value_round_trips() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // Seed the key. SET_METADATA does not produce a reply, so we
        // pipeline it before the GET — the server processes frames in
        // order on a single connection.
        send_frame(
            &mut stream,
            &FrameKind::SetMetadata {
                request_id: 1,
                scope: collection_scope(),
                key: LAYOUT_KEY.to_owned(),
                value: LAYOUT_VALUE.to_vec(),
            },
        )
        .await;

        // GET it back.
        send_frame(
            &mut stream,
            &FrameKind::GetMetadata {
                request_id: 0xCAFE_F00D,
                scope: collection_scope(),
                key: LAYOUT_KEY.to_owned(),
            },
        )
        .await;

        let (type_byte, reply) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_METADATA_VALUE,
            "GET_METADATA must elicit METADATA_VALUE (got type 0x{type_byte:02x})",
        );
        match reply {
            FrameKind::MetadataValue { request_id, value } => {
                assert_eq!(request_id, 0xCAFE_F00D, "request_id must echo verbatim");
                assert_eq!(
                    value.as_deref(),
                    Some(LAYOUT_VALUE),
                    "value must round-trip the bytes we set",
                );
            }
            other => panic!("expected MetadataValue, got {other:?}"),
        }

        // Clean shutdown.
        drop(stream);
        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
    });
}

#[test]
fn metadata_get_reply_absent_key_returns_none() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        send_frame(
            &mut stream,
            &FrameKind::GetMetadata {
                request_id: 0xDEAD_BEEF,
                scope: collection_scope(),
                key: "phux.never.set/v1".to_owned(),
            },
        )
        .await;

        let (type_byte, reply) = recv_typed(&mut stream).await;
        assert_eq!(type_byte, TYPE_METADATA_VALUE);
        match reply {
            FrameKind::MetadataValue { request_id, value } => {
                assert_eq!(request_id, 0xDEAD_BEEF);
                assert!(value.is_none(), "absent key must reply with value: None");
            }
            other => panic!("expected MetadataValue, got {other:?}"),
        }

        drop(stream);
        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
    });
}

#[test]
fn metadata_list_reply_returns_sorted_keys() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);

        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // Seed two keys, deliberately in non-sorted order, to verify the
        // server sorts before replying (MetadataStore::list does this;
        // the test guards against a future regression).
        send_frame(
            &mut stream,
            &FrameKind::SetMetadata {
                request_id: 1,
                scope: collection_scope(),
                key: OTHER_KEY.to_owned(),
                value: b"o".to_vec(),
            },
        )
        .await;
        send_frame(
            &mut stream,
            &FrameKind::SetMetadata {
                request_id: 2,
                scope: collection_scope(),
                key: LAYOUT_KEY.to_owned(),
                value: LAYOUT_VALUE.to_vec(),
            },
        )
        .await;

        send_frame(
            &mut stream,
            &FrameKind::ListMetadata {
                request_id: 0x0000_0099,
                scope: collection_scope(),
            },
        )
        .await;

        let (type_byte, reply) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_METADATA_KEYS,
            "LIST_METADATA must elicit METADATA_KEYS (got type 0x{type_byte:02x})",
        );
        match reply {
            FrameKind::MetadataKeys { request_id, keys } => {
                assert_eq!(request_id, 0x0000_0099);
                assert_eq!(
                    keys,
                    vec![LAYOUT_KEY.to_owned(), OTHER_KEY.to_owned()],
                    "keys must be lexicographically sorted",
                );
            }
            other => panic!("expected MetadataKeys, got {other:?}"),
        }

        drop(stream);
        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
    });
}
