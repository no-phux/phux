//! Helpers shared by the metadata and event suites.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` demands pub(crate) in a test-binary module"
)]

use std::path::{Path, PathBuf};

use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet, ServerFeatureExt};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{CommandResult, FrameKind};
use phux_server::LaunchCommand as CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, SpawnedServer, attach_by_name, command, recv_typed, recv_until,
    send_frame, wait_for_raw_socket,
};

/// Every layer, true color: what a full client advertises.
pub(crate) const fn full_caps() -> ClientCapabilities {
    ClientCapabilities::new()
        .with_color_support(ColorSupport::TrueColor)
        .with_layers(LayerSet::all())
}

/// Connect and HELLO as `name` with `caps`; returns the stream and `HELLO_OK`.
pub(crate) async fn connect_as(
    socket: &Path,
    name: &str,
    caps: ClientCapabilities,
) -> (UnixStream, FrameKind) {
    let mut stream = wait_for_raw_socket(socket, SOCKET_CONNECT_DEADLINE).await;
    send_frame(&mut stream, &phux_server_testkit::hello(name, caps)).await;
    let (_, hello_ok) = recv_typed(&mut stream).await;
    assert!(
        matches!(hello_ok, FrameKind::HelloOk { .. }),
        "{hello_ok:?}"
    );
    (stream, hello_ok)
}

/// Whether `hello_ok` (a `HELLO_OK`) advertises `feature`.
pub(crate) fn advertises(hello_ok: &FrameKind, feature: ServerFeatureExt) -> bool {
    let FrameKind::HelloOk { server_caps, .. } = hello_ok else {
        panic!("expected HELLO_OK: {hello_ok:?}");
    };
    server_caps.features_ext.contains(feature)
}

/// Attach to `session` and return its focused pane.
pub(crate) async fn attach_pane(stream: &mut UnixStream, session: &str) -> ResourceId {
    send_frame(stream, &attach_by_name(session)).await;
    recv_until(stream, |_, frame| match frame {
        FrameKind::Attached { snapshot, .. } => Some(snapshot.focused_resource),
        _ => None,
    })
    .await
}

/// `/bin/sh -c` that emits nothing until `release` exists, then runs `script`.
/// Tests write the file only after the server has installed their
/// subscription, so no startup latency can let the pane speak first.
pub(crate) fn gated_seed(release: &Path, script: &str) -> CommandBuilder {
    phux_server_testkit::sh(&format!(
        "until [ -f '{}' ]; do sleep 0.01; done; {script}",
        release.display(),
    ))
}

/// A server seeding session `demo` with [`gated_seed`] over `script`, and
/// the release file to write once the test's subscription is installed.
pub(crate) fn gated_server(script: &str) -> (SpawnedServer, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let release = tmp.path().join("release");
    let seed = gated_seed(&release, script);
    (SpawnedServer::seeded_in(tmp, "demo", seed), release)
}

/// `SUBSCRIBE_EVENTS { terminal: None }` plus a `GET_STATE` barrier: frames on
/// one connection are handled in order, so the reply proves the subscription
/// is installed (the subscribe itself is answered with no frame).
pub(crate) async fn subscribe_all(stream: &mut UnixStream, request_id: u32) {
    send_frame(stream, &phux_server_testkit::subscribe_events(None, None)).await;
    let get_state = phux_server_testkit::get_state();
    let barrier = command(stream, request_id, get_state).await;
    assert!(
        !matches!(barrier, CommandResult::Error { .. }),
        "{barrier:?}"
    );
}
