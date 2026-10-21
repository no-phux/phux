//! A newer 1.x server's additions on a [`Session`]: the TUI and the CLI read
//! through here, so an unknown frame type or enum value must skip that one
//! frame, and a correlated reply it cannot fully read must still settle.

#![allow(clippy::expect_used, reason = "test assertions")]

use std::time::Duration;

use phux_protocol::ids::{GroupId, ResourceId};
use phux_protocol::wire::frame::{
    FrameKind, SpawnError, SpawnResult, TYPE_METADATA_CHANGED, TYPE_RESOURCE_SPAWNED,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use super::{Session, SessionError, encode};

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(fut)
}

/// A frame of `type_byte` with the given `(field id, value)` TLV fields.
fn raw(type_byte: u8, fields: &[(u8, &[u8])]) -> Vec<u8> {
    let mut body = vec![type_byte];
    for (id, value) in fields {
        // Every id and length here is under 128: one varint byte each.
        body.extend([*id, 2, u8::try_from(value.len()).expect("short")]);
        body.extend_from_slice(value);
    }
    let mut frame = u32::try_from(body.len())
        .expect("short")
        .to_be_bytes()
        .to_vec();
    frame.extend(body);
    frame
}

fn newer_frames() -> Vec<u8> {
    let mut bytes = raw(0x6E, &[]);
    bytes.extend(raw(TYPE_METADATA_CHANGED, &[(1, &[0xFE]), (2, b"key")]));
    bytes
}

fn bell() -> FrameKind {
    FrameKind::Bell {
        terminal_id: ResourceId::local(1),
    }
}

#[test]
fn recv_and_try_recv_skip_a_newer_servers_frames() {
    block_on(async {
        let (near, mut peer) = UnixStream::pair().expect("socketpair");
        let mut client = Session::from_stream(near);
        let mut burst = newer_frames();
        burst.extend(encode(&bell()));
        burst.extend(newer_frames());
        burst.extend(encode(&bell()));
        peer.write_all(&burst).await.expect("burst");
        assert_eq!(client.recv().await.expect("first known frame"), bell());
        let second = loop {
            if let Some(frame) = client.try_recv().expect("try_recv") {
                break frame;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(second, bell());
        // Malformed bytes are still a protocol error.
        peer.write_all(&raw(TYPE_RESOURCE_SPAWNED, &[(1, &[0, 0])]))
            .await
            .expect("malformed");
        assert!(matches!(
            client.recv().await,
            Err(SessionError::Protocol(_))
        ));
    });
}

#[test]
fn a_spawn_answered_with_an_unknown_refusal_settles() {
    block_on(async {
        let (near, mut peer) = UnixStream::pair().expect("socketpair");
        let mut client = Session::from_stream(near);
        let spawn = FrameKind::SpawnResource {
            request_id: 7,
            group: GroupId::new(1),
            command: None,
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: None,
        };
        let server = async {
            let mut request = [0_u8; 1];
            peer.read_exact(&mut request).await.expect("request");
            let mut reply = newer_frames();
            reply.extend(raw(
                TYPE_RESOURCE_SPAWNED,
                &[(1, &7_u32.to_be_bytes()), (2, &[0x01, 0xFE])],
            ));
            peer.write_all(&reply).await.expect("reply");
            peer
        };
        let (reply, _peer) = tokio::join!(client.request_spawn(&spawn), server);
        let reply = reply.expect("the spawn settles");
        assert_eq!(
            reply.result(),
            &Ok(SpawnResult::Err(SpawnError::Unknown(0xFE)))
        );
    });
}
