//! `phux.whoami/v1` over a real Unix socket (`docs/spec/L3.md` §3.9,
//! ADR-0106): the feature is advertised, `GET_METADATA` answers the asking
//! connection's own identity with the kernel peer uid populated, and a
//! client `SET_METADATA` or `DELETE_METADATA` of the key changes nothing.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet, ServerFeature};
use phux_protocol::wire::frame::{
    FrameKind, Scope, TYPE_HELLO_OK, TYPE_METADATA_VALUE, WHOAMI_KEY, WhoamiRecord,
};
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, recv_typed, run_local, send_frame, spawn_server_with_seed_cmd,
    wait_for_raw_socket,
};

async fn connect(path: &std::path::Path) -> UnixStream {
    let mut stream = wait_for_raw_socket(path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &FrameKind::Hello {
            client_name: "whoami-test".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new()
                .with_color_support(ColorSupport::TrueColor)
                .with_layers(LayerSet::all()),
        },
    )
    .await;
    let (type_byte, frame) = recv_typed(&mut stream).await;
    assert_eq!(type_byte, TYPE_HELLO_OK, "HELLO must be accepted");
    let FrameKind::HelloOk { server_caps, .. } = frame else {
        panic!("expected HELLO_OK, got {frame:?}");
    };
    assert!(
        server_caps.features.contains(ServerFeature::Whoami),
        "server must advertise WHOAMI: {server_caps:?}"
    );
    stream
}

/// One correlated `GET_METADATA` of `key` under `scope`.
async fn get(stream: &mut UnixStream, request_id: u32, scope: Scope) -> Option<Vec<u8>> {
    send_frame(
        stream,
        &FrameKind::GetMetadata {
            request_id,
            scope,
            key: WHOAMI_KEY.to_owned(),
        },
    )
    .await;
    loop {
        let (type_byte, frame) = recv_typed(stream).await;
        if type_byte != TYPE_METADATA_VALUE {
            continue;
        }
        let FrameKind::MetadataValue {
            request_id: got,
            value,
        } = frame
        else {
            panic!("expected METADATA_VALUE, got {frame:?}");
        };
        assert_eq!(got, request_id, "reply must correlate to its request");
        return value;
    }
}

async fn whoami(stream: &mut UnixStream, request_id: u32) -> WhoamiRecord {
    let bytes = get(stream, request_id, Scope::Global)
        .await
        .expect("the whoami key always answers on a WHOAMI server");
    serde_json::from_slice(&bytes).expect("the value is the documented JSON record")
}

#[test]
fn whoami_reports_the_uds_peer_and_refuses_client_writes() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("sleep 30");
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), "whoami", cmd);

        let mut stream = connect(&socket_path).await;
        let me = nix::unistd::getuid().as_raw();

        let record = whoami(&mut stream, 1).await;
        assert_eq!(record.schema_version, 1);
        assert_eq!(record.auth_route, "uds");
        assert_eq!(record.peer_uid, Some(me), "the kernel peer uid is reported");
        assert_eq!(
            record.principal, None,
            "a UDS client presents no credential"
        );
        assert_eq!(record.credential_id, None);
        assert_eq!(
            record.serving_user.uid, me,
            "the server runs as the user that started it"
        );
        assert!(!record.server_version.is_empty());

        // A client write is refused: the next read is still the computed
        // record, not the forged bytes, and a delete removes nothing.
        send_frame(
            &mut stream,
            &FrameKind::SetMetadata {
                request_id: 2,
                scope: Scope::Global,
                key: WHOAMI_KEY.to_owned(),
                value: br#"{"principal":"root"}"#.to_vec(),
            },
        )
        .await;
        assert_eq!(whoami(&mut stream, 3).await, record, "SET must not land");
        send_frame(
            &mut stream,
            &FrameKind::DeleteMetadata {
                request_id: 4,
                scope: Scope::Global,
                key: WHOAMI_KEY.to_owned(),
            },
        )
        .await;
        assert_eq!(whoami(&mut stream, 5).await, record, "DELETE must not land");

        // The key is Global-only; under a Terminal scope it is just an unset
        // ordinary key.
        assert_eq!(
            get(
                &mut stream,
                6,
                Scope::Resource(phux_protocol::ids::ResourceId::local(1))
            )
            .await,
            None
        );

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}
