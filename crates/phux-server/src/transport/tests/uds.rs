use super::*;

/// The client loop drops `read_frame` whenever another `select!` arm
/// wins. A frame split across that drop must still arrive whole, or the
/// stream desynchronises and payload bytes are parsed as frames.
#[tokio::test]
async fn a_dropped_read_loses_no_bytes() {
    let (client, server) = tokio::net::UnixStream::pair().unwrap();
    let (server_read, _server_write) = server.into_split();
    let mut reader = UdsReader {
        reader: server_read,
        frames: FrameAssembler::default(),
    };
    let mut client = client;
    let frame = [0_u8, 0, 0, 6, 0xaa, 1, 2, 3, 4, 5];
    client.write_all(&frame[..7]).await.unwrap();
    let cancelled = tokio::time::timeout(Duration::from_millis(50), reader.read_frame()).await;
    assert!(cancelled.is_err(), "half a frame must not complete a read");
    client.write_all(&frame[7..]).await.unwrap();
    let read = tokio::time::timeout(Duration::from_secs(5), reader.read_frame())
        .await
        .expect("the rest of the frame completes the read")
        .unwrap()
        .expect("one frame");
    assert_eq!(&read[..], &frame[..]);
    drop(client);
    assert!(reader.read_frame().await.unwrap().is_none(), "clean EOF");
}

/// A bare length header commits no more memory than the bytes that
/// actually arrived: a peer cannot pin a maximum-size frame buffer with
/// four bytes.
#[tokio::test]
async fn a_bare_header_does_not_reserve_the_whole_frame() {
    let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
    let (server_read, _server_write) = server.into_split();
    let mut reader = UdsReader {
        reader: server_read,
        frames: FrameAssembler::default(),
    };
    let max = phux_protocol::wire::frame::MAX_FRAME_LEN;
    client.write_all(&max.to_be_bytes()).await.unwrap();
    let pending = tokio::time::timeout(Duration::from_millis(50), reader.read_frame()).await;
    assert!(pending.is_err());
    assert!(
        reader.frames.buf.capacity() < 1024 * 1024,
        "reserved {} bytes for a 4-byte header",
        reader.frames.buf.capacity()
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn uds_peer_identity_contains_kernel_attested_process_id() {
    let directory = tempfile::tempdir().expect("private socket directory");
    let socket = directory.path().join("peer.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("bind socket");
    let _client = tokio::net::UnixStream::connect(&socket)
        .await
        .expect("connect socket");
    let (stream, _) = listener.accept().await.expect("accept socket");
    let peer = peer_identity_from_uds(&stream).expect("kernel peer credentials");
    assert_eq!(peer.uid, rustix::process::geteuid().as_raw());
    assert_eq!(peer.pid, Some(std::process::id()));
}

#[test]
fn uds_credential_failure_is_never_root_fallback() {
    let error = peer_identity_from_credentials(Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "simulated peer credential failure",
    )))
    .expect_err("missing authenticated credentials must fail closed");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}
