//! phux-w7z2.55: `DETACH` ends an attachment, not the connection, so HELLO's
//! negotiated layers and the transport identity must survive it. Losing the
//! layers promoted an L1-only consumer to the permissive `LayerSet::all()`
//! default (fail-open); losing the identity made `SHUTDOWN`'s local-socket
//! gate refuse a local operator (fail-closed).

use phux_protocol::caps::{ClientCapabilities, LayerSet};
use phux_protocol::ids::GroupId;
use phux_protocol::wire::frame::{
    Command, CommandResult, FrameKind, Scope, TYPE_ATTACH_READY, TYPE_METADATA_VALUE, TYPE_PONG,
};
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{
    attach_by_name, command, recv_until, recv_until_detached, run_local, send_frame, spawn_server,
};

use super::common::connect_with;

async fn attach_then_detach(path: &std::path::Path, layers: LayerSet) -> UnixStream {
    let mut stream = connect_with(path, ClientCapabilities::new().with_layers(layers)).await;
    send_frame(&mut stream, &attach_by_name("work")).await;
    recv_until(&mut stream, |type_byte, _| {
        (type_byte == TYPE_ATTACH_READY).then_some(())
    })
    .await;
    send_frame(&mut stream, &FrameKind::Detach).await;
    recv_until_detached(&mut stream).await;
    stream
}

#[test]
fn l1_only_consumer_still_fails_the_l3_gate_after_detach() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (_shutdown, _server) = spawn_server(socket.clone(), Some("work"));
        let mut stream = attach_then_detach(&socket, LayerSet::new()).await;

        // A non-L3 consumer's GET_METADATA is dropped silently (§11.5); the
        // in-order PONG proves it was already dispatched.
        let get = FrameKind::GetMetadata {
            request_id: 7,
            scope: Scope::Group(GroupId::new(1)),
            key: "phux.tui.layout/v1".to_owned(),
        };
        send_frame(&mut stream, &get).await;
        send_frame(&mut stream, &FrameKind::Ping { nonce: 0x7255 }).await;
        recv_until(&mut stream, |type_byte, frame| {
            assert_ne!(
                type_byte, TYPE_METADATA_VALUE,
                "L3 reply after DETACH: {frame:?}"
            );
            (type_byte == TYPE_PONG).then_some(())
        })
        .await;
    });
}

#[test]
fn local_peer_can_still_shut_down_after_detach() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (_shutdown, server) = spawn_server(socket.clone(), Some("work"));
        let mut stream = attach_then_detach(&socket, LayerSet::all()).await;
        assert_eq!(
            command(&mut stream, 11, Command::Shutdown).await,
            CommandResult::Ok
        );
        drop(stream);
        server.await.unwrap().unwrap();
    });
}
