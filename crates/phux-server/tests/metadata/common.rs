//! Helpers shared by the metadata and event suites.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` demands pub(crate) in a test-binary module"
)]

use std::path::Path;

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{Command, CommandResult, FrameKind, StateScope};
use portable_pty::CommandBuilder;
use tokio::net::UnixStream;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, command, recv_typed, recv_until, send_frame,
    wait_for_raw_socket,
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
    send_frame(
        &mut stream,
        &FrameKind::Hello {
            client_name: name.to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: caps,
        },
    )
    .await;
    let (_, hello_ok) = recv_typed(&mut stream).await;
    assert!(
        matches!(hello_ok, FrameKind::HelloOk { .. }),
        "{hello_ok:?}"
    );
    (stream, hello_ok)
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
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg(format!(
        "until [ -f '{}' ]; do sleep 0.01; done; {script}",
        release.display(),
    ));
    cmd
}

/// `SUBSCRIBE_EVENTS { terminal: None }` plus a `GET_STATE` barrier: frames on
/// one connection are handled in order, so the reply proves the subscription
/// is installed (the subscribe itself is answered with no frame).
pub(crate) async fn subscribe_all(stream: &mut UnixStream, request_id: u32) {
    send_frame(
        stream,
        &FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: None,
        },
    )
    .await;
    let get_state = Command::GetState {
        scope: StateScope::Server,
    };
    let barrier = command(stream, request_id, get_state).await;
    assert!(
        !matches!(barrier, CommandResult::Error { .. }),
        "{barrier:?}"
    );
}
