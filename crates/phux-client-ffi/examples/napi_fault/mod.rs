//! A real UDS proxy with one deterministic fault: withhold the first
//! `APPLY_INPUT` and sever the connection before any receipt can arrive. In
//! close mode no second connection is served; in restart mode a new real
//! server incarnation serves the reconnect. The marker only coordinates the
//! test; no mock events or runtime state are injected.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use phux_protocol::wire::frame::{Command, FrameKind};
use phux_server_testkit::{
    SERVER_JOIN_DEADLINE, SOCKET_CONNECT_DEADLINE, spawn_server_with, wait_for_raw_socket,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixListener;

#[allow(
    clippy::redundant_pub_crate,
    reason = "private binary fixture entry point, not an externally reachable API"
)]
pub(super) async fn run(addon: &str, restart: bool) {
    let tmp = tempfile::TempDir::new().expect("fault fixture directory");
    let backend_path = tmp.path().join("server.sock");
    let proxy_path = tmp.path().join("proxy.sock");
    let marker = tmp.path().join("input-withheld");
    let (mut shutdown, mut server) =
        spawn_server_with(backend_path.clone(), Some("napi-smoke"), |config| {
            config.seed_with_pty = true;
        });
    let listener = Arc::new(UnixListener::bind(&proxy_path).expect("proxy bind"));
    let first =
        tokio::task::spawn_local(intercept_connection(listener.clone(), backend_path.clone()));
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/napi-fault.cjs");
    let mut child = tokio::process::Command::new("node")
        .arg(script)
        .arg(addon)
        .arg(proxy_path)
        .arg(&marker)
        .arg(if restart { "restart" } else { "close" })
        .kill_on_drop(true)
        .spawn()
        .expect("fault node");
    first.await.expect("intercepted an actual APPLY_INPUT");
    std::fs::write(marker, "withheld").expect("notify node of intercepted input");
    let relay = if restart {
        shutdown.send(()).expect("stop first incarnation");
        tokio::time::timeout(SERVER_JOIN_DEADLINE, server)
            .await
            .expect("old server deadline")
            .expect("old server join")
            .expect("old server shutdown");
        (shutdown, server) =
            spawn_server_with(backend_path.clone(), Some("napi-smoke"), |config| {
                config.seed_with_pty = true;
            });
        Some(tokio::task::spawn_local(relay_connection(
            listener.clone(),
            backend_path,
        )))
    } else {
        None
    };
    let status = child.wait().await.expect("fault node exit");
    if let Some(relay) = relay {
        tokio::time::timeout(Duration::from_secs(10), relay)
            .await
            .expect("relay stop deadline")
            .expect("relay join");
    }
    let _ = shutdown.send(());
    tokio::time::timeout(SERVER_JOIN_DEADLINE, server)
        .await
        .expect("server stop deadline")
        .expect("server join")
        .expect("server shutdown");
    assert!(status.success(), "fault JS failed: {status}");
}

async fn intercept_connection(listener: Arc<UnixListener>, backend_path: PathBuf) {
    let (mut frontend, _) = listener.accept().await.expect("first client");
    let mut backend = wait_for_raw_socket(&backend_path, SOCKET_CONNECT_DEADLINE).await;
    let (mut front_read, mut front_write) = frontend.split();
    let (mut back_read, mut back_write) = backend.split();
    tokio::select! {
        () = forward_until_input(&mut front_read, &mut back_write) => {},
        result = tokio::io::copy(&mut back_read, &mut front_write) => {
            panic!("server ended before APPLY_INPUT: {result:?}");
        }
    }
}

async fn forward_until_input(
    reader: &mut (impl AsyncRead + Unpin),
    writer: &mut (impl AsyncWrite + Unpin),
) {
    loop {
        let frame = read_frame(reader).await;
        let (kind, remaining) = FrameKind::decode(&frame).expect("valid client frame");
        assert!(remaining.is_empty());
        if matches!(
            kind,
            FrameKind::Command {
                command: Command::ApplyInput { .. },
                ..
            }
        ) {
            return;
        }
        writer
            .write_all(&frame)
            .await
            .expect("forward client frame");
    }
}

async fn read_frame(reader: &mut (impl AsyncRead + Unpin)) -> Vec<u8> {
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header).await.expect("frame header");
    let len = usize::try_from(u32::from_be_bytes(header)).expect("frame length");
    assert!(len < 1024 * 1024, "bounded fixture client frame");
    let mut frame = vec![0; len + 4];
    frame[..4].copy_from_slice(&header);
    reader
        .read_exact(&mut frame[4..])
        .await
        .expect("frame body");
    frame
}

async fn relay_connection(listener: Arc<UnixListener>, backend_path: PathBuf) {
    let (mut frontend, _) = listener.accept().await.expect("reconnected client");
    let mut backend = wait_for_raw_socket(&backend_path, SOCKET_CONNECT_DEADLINE).await;
    // Explicit client close can race a final server frame in this proxy.
    if let Err(error) = tokio::io::copy_bidirectional(&mut frontend, &mut backend).await {
        assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
            ),
            "reconnect relay: {error}"
        );
    }
}
