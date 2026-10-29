//! Session behavior against a scripted peer on a socket pair.

#![allow(clippy::expect_used, reason = "test assertions")]

use std::io;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapStreamProfile, select_bootstrap_profile,
};
use phux_protocol::ids::{BootstrapId, GroupId, ResourceId, SatelliteHost, StreamId};
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, MAX_FRAME_LEN, Scope, SpawnResult, StateScope,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

mod correlation;
mod handshake;
mod metadata;
mod transport;

/// A hanging wait is the defect under test, so every round trip is capped.
const WEDGE_TIMEOUT: Duration = Duration::from_secs(20);

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    tokio::task::LocalSet::new().block_on(&rt, fut)
}

/// A frame carrying `seq`, for ordering assertions.
fn seq_ack(seq: u64) -> FrameKind {
    FrameKind::FrameAck {
        terminal_id: ResourceId::local(1),
        stream_id: StreamId::new(1).expect("stream"),
        bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
        seq,
    }
}

fn pair() -> (Session, UnixStream) {
    let (near, far) = UnixStream::pair().expect("socketpair");
    (Session::from_stream(near), far)
}

/// Every frame `peer` holds once `expected` bytes have arrived.
async fn read_frames(peer: &mut UnixStream, expected: usize) -> Vec<FrameKind> {
    let mut buf = BytesMut::new();
    while buf.len() < expected {
        assert!(
            peer.read_buf(&mut buf).await.expect("read") > 0,
            "peer closed"
        );
    }
    let mut frames = Vec::new();
    while let Some(frame) = framing::split_frame(&mut buf).expect("well-formed") {
        frames.push(FrameKind::decode(&frame).expect("decodes").0);
    }
    frames
}

/// Spawn a server half that plays `script` once the request arrives.
fn scripted(script: Vec<FrameKind>) -> Session {
    let (client, server) = UnixStream::pair().expect("pair");
    let mut server = Session::from_stream(server);
    tokio::task::spawn_local(async move {
        server.recv().await.expect("request frame");
        for frame in &script {
            server.send(frame).await.expect("scripted frame");
        }
    });
    Session::from_stream(client)
}

fn ack(request_id: u32) -> FrameKind {
    FrameKind::CommandResult {
        request_id,
        result: CommandResult::Ok,
    }
}

fn bootstrap() -> Vec<FrameKind> {
    let terminal_id = ResourceId::local(1);
    let stream_id = StreamId::new(1).expect("stream");
    let bootstrap_id = BootstrapId::new(1).expect("bootstrap");
    vec![
        FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 120,
            rows: 40,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: Bytes::from_static(b"opening screen"),
        },
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ]
}

/// One `COMMAND` round trip against `script`.
fn request_against(script: Vec<FrameKind>) -> Reply {
    block_on(async {
        let mut client = scripted(script);
        let command = Command::GetState {
            scope: StateScope::Server,
        };
        tokio::time::timeout(WEDGE_TIMEOUT, client.request(7, command))
            .await
            .expect("the request must resolve")
            .expect("reply")
    })
}

// ----- handshake ----------------------------------------------------------

fn hello_ok(
    offered: &ClientCapabilities,
    server_caps: phux_protocol::caps::ServerCapabilities,
) -> FrameKind {
    let (selected_profile, bootstrap_limits) =
        select_bootstrap_profile(offered, &BootstrapCapabilities::new()).expect("intersect");
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps,
        server_id: b"incarnation-1".to_vec(),
        selected_profile,
        bootstrap_limits,
    }
}

/// Answer the HELLO that arrives with `answer(offer)`.
fn negotiate_against(
    answer: impl FnOnce(&ClientCapabilities) -> FrameKind + 'static,
) -> (Session, Result<(), SessionError>) {
    block_on(async {
        let (mut client, server) = pair();
        let mut server = Session::from_stream(server);
        let peer = tokio::task::spawn_local(async move {
            let FrameKind::Hello { client_caps, .. } = server.recv().await.expect("HELLO") else {
                panic!("the first frame is HELLO");
            };
            server.send(&answer(&client_caps)).await.expect("answer");
            server
        });
        let negotiated = client.negotiate("probe".to_owned(), control_caps()).await;
        drop(peer.await.expect("peer"));
        (client, negotiated)
    })
}

fn refusal(request_id: u32) -> FrameKind {
    FrameKind::Error {
        request_id: Some(request_id),
        code: ErrorCode::PermissionDenied,
        message: "policy refused that scope".to_owned(),
    }
}
