//! A server on another minor or patch of this client's protocol major: the
//! connection accepts it and keeps the server's version, not this build's.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "tests"
)]

use bytes::BytesMut;
use phux_client::attach::connection::Connection;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{BootstrapCapabilities, ServerCapabilities, select_bootstrap_profile};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::framing::{self, LENGTH_PREFIX_LEN};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

async fn recv(stream: &mut UnixStream) -> FrameKind {
    let mut header = [0; LENGTH_PREFIX_LEN];
    stream.read_exact(&mut header).await.unwrap();
    let mut encoded = framing::frame_buffer(header).unwrap();
    stream
        .read_exact(&mut encoded[LENGTH_PREFIX_LEN..])
        .await
        .unwrap();
    FrameKind::decode(&encoded).unwrap().0
}

/// Answer one `HELLO` with `HELLO_OK` on `protocol`, then hold the stream.
async fn answer_hello(listener: UnixListener, protocol: (u16, u16, u16)) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let FrameKind::Hello { client_caps, .. } = recv(&mut stream).await else {
        panic!("the client opens with HELLO");
    };
    let (selected_profile, bootstrap_limits) =
        select_bootstrap_profile(&client_caps, &BootstrapCapabilities::new()).unwrap();
    let mut encoded = BytesMut::new();
    FrameKind::HelloOk {
        protocol_major: protocol.0,
        protocol_minor: protocol.1,
        protocol_patch: protocol.2,
        server_caps: ServerCapabilities::new(),
        server_id: vec![1; 16],
        selected_profile,
        bootstrap_limits,
    }
    .encode(&mut encoded);
    stream.write_all(&encoded).await.unwrap();
    // Stay open until the client hangs up.
    let mut rest = Vec::new();
    let _ = stream.read_to_end(&mut rest).await;
}

#[tokio::test]
async fn a_newer_minor_is_accepted_and_reported_as_the_servers() {
    let v = PROTOCOL_VERSION;
    let newer = (v.major, v.minor + 1, v.patch + 2);
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("phux.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(answer_hello(listener, newer));

    let mut conn = Connection::connect(&socket).await.expect("same major");
    assert_eq!(conn.server_protocol(), Some(newer), "kept from HELLO_OK");
    assert_eq!(
        phux_client::state::probe_hello(&mut conn).await.unwrap(),
        newer,
        "the server's version, not this client's"
    );
    drop(conn);
    server.await.unwrap();
}
