//! `phux.whoami/v1` over a real Unix socket (`docs/spec/L3.md` §3.9,
//! ADR-0106): `GET_METADATA` answers the asking connection's own identity,
//! client writes of the key change nothing, and a bridge-announced
//! `ssh_origin` reports `ssh-stdio` (another uid's announcement is refused in
//! `runtime::whoami`'s unit tests, since a test cannot change uid).

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, Scope, SshClient, WHOAMI_KEY, WhoamiRecord};
use phux_protocol::wire::ssh_origin::SshOrigin;
use tokio::net::UnixStream;

use phux_server_testkit::{recv_until, run_local, send_frame, spawn_server};

use crate::common::{connect_as, full_caps};

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
    recv_until(stream, |_, frame| match frame {
        FrameKind::MetadataValue {
            request_id: got,
            value,
        } if got == request_id => Some(value),
        _ => None,
    })
    .await
}

async fn whoami(stream: &mut UnixStream, request_id: u32) -> WhoamiRecord {
    let bytes = get(stream, request_id, Scope::Global)
        .await
        .expect("the whoami key always answers");
    serde_json::from_slice(&bytes).expect("the documented JSON record")
}

#[test]
fn whoami_reports_each_connections_route_and_refuses_client_writes() {
    run_local(async {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (_shutdown, _server) = spawn_server(socket_path.clone(), Some("whoami"));
        let me = nix::unistd::getuid().as_raw();

        let (mut plain, _) = connect_as(&socket_path, "whoami", full_caps()).await;
        let record = whoami(&mut plain, 1).await;
        assert_eq!(record.schema_version, 1);
        assert_eq!(record.auth_route, "uds");
        assert_eq!(record.peer_uid, Some(me), "the kernel peer uid");
        assert_eq!(
            (record.principal.as_deref(), record.credential_id.as_deref()),
            (None, None)
        );
        assert_eq!(record.ssh_client, None);
        assert_eq!(record.serving_user.uid, me);
        assert!(!record.server_version.is_empty());

        // Client SET and DELETE never land; the key is Global-only.
        send_frame(
            &mut plain,
            &FrameKind::SetMetadata {
                request_id: 2,
                scope: Scope::Global,
                key: WHOAMI_KEY.to_owned(),
                value: br#"{"principal":"root"}"#.to_vec(),
            },
        )
        .await;
        assert_eq!(whoami(&mut plain, 3).await, record, "SET must not land");
        send_frame(
            &mut plain,
            &FrameKind::DeleteMetadata {
                request_id: 4,
                scope: Scope::Global,
                key: WHOAMI_KEY.to_owned(),
            },
        )
        .await;
        assert_eq!(whoami(&mut plain, 5).await, record, "DELETE must not land");
        assert_eq!(
            get(&mut plain, 6, Scope::Resource(ResourceId::local(1))).await,
            None
        );

        let origin = SshOrigin {
            client: "203.0.113.5:52144".parse().unwrap(),
            server: Some("198.51.100.7:22".parse().unwrap()),
        };
        let (mut bridged, _) =
            connect_as(&socket_path, "whoami", full_caps().with_ssh_origin(origin)).await;
        let record = whoami(&mut bridged, 1).await;
        assert_eq!(record.auth_route, "ssh-stdio");
        assert_eq!(
            record.ssh_client,
            Some(SshClient {
                addr: "203.0.113.5".to_owned(),
                port: 52144,
            })
        );
        assert_eq!(record.peer_uid, Some(me), "still the bridge's kernel uid");
        assert_eq!(record.principal, None, "the announcement grants nothing");
    });
}
