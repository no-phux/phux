//! Real-QUIC acceptance for a pane spawned by an attached multi-stream client.
//!
//! The TUI's `new-window` and splits are `SPAWN_RESOURCE` from an attached
//! connection. Under `QUIC_STREAMS` every L1 §4 frame rides the pane's own
//! Terminal stream (L1 §4.9), so the spawned pane's content must wait for the
//! client's `STREAM_BIND` rather than leak onto control, and once bound the
//! pane must take the resize and input the TUI sends it straight away.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::future_not_send, reason = "ServerRuntime owns LocalSet actors")]

mod support;

use std::time::Duration;

use phux_client::attach::connection::Connection;
use phux_protocol::ids::ResourceId;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{AttachTarget, FrameKind, SpawnResult, ViewportInfo};
use phux_server::DEFAULT_GROUP_ID;
use support::{STEP_DEADLINE, Server, dial, quic_env, seeded_config};
use tempfile::TempDir;
use tokio::time::{Instant, timeout};

const SESSION: &str = "quic-spawn";
const SPAWN_REQUEST: u32 = 7;
const FENCE_NONCE: u64 = 0x5eed_f00d;

async fn recv_step(connection: &mut Connection) -> FrameKind {
    timeout(STEP_DEADLINE, connection.recv())
        .await
        .expect("QUIC receive timed out")
        .expect("QUIC receive failed")
}

/// The Terminal a content frame belongs to, if it is one.
const fn content_terminal(frame: &FrameKind) -> Option<&ResourceId> {
    match frame {
        FrameKind::BootstrapBegin { terminal_id, .. }
        | FrameKind::BootstrapChunk { terminal_id, .. }
        | FrameKind::BootstrapReady { terminal_id, .. }
        | FrameKind::BootstrapTombstone { terminal_id, .. }
        | FrameKind::ResourceOutput { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}

/// Session ATTACH and bind every pane in the snapshot, as the TUI does.
async fn attach_and_bind(connection: &mut Connection) {
    let attach_id = connection.next_attach_id();
    connection
        .send(&FrameKind::Attach {
            attach_id,
            target: AttachTarget::ByName(SESSION.to_owned()),
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: true,
            scrollback_limit_lines: 1_000,
            role_policy: None,
        })
        .await
        .expect("send ATTACH");
    let snapshot = loop {
        if let FrameKind::Attached { snapshot, .. } = recv_step(connection).await {
            break snapshot;
        }
    };
    for resource in &snapshot.resources {
        connection
            .bind_terminal(&resource.id)
            .await
            .expect("bind session pane");
    }
}

/// Spawn a pane the way the TUI's `new-window` does and return its id,
/// failing if any of its content reaches the client before it is bound.
async fn spawn_unbound_pane(connection: &mut Connection) -> ResourceId {
    connection
        .send(&FrameKind::SpawnResource {
            request_id: SPAWN_REQUEST,
            group: DEFAULT_GROUP_ID,
            command: Some(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "stty -echo; while IFS= read -r line; do printf 'got:%s\\n' \"$line\"; done"
                    .to_owned(),
            ]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: Some((80, 24)),
            resource: None,
        })
        .await
        .expect("send SPAWN_RESOURCE");
    let spawned = loop {
        if let FrameKind::ResourceSpawned {
            request_id: SPAWN_REQUEST,
            result,
        } = recv_step(connection).await
        {
            match result {
                SpawnResult::Ok(id) => break id,
                other => panic!("SPAWN_RESOURCE failed: {other:?}"),
            }
        }
    };
    // Control is processed in order, so the PONG to a PING sent now lands
    // after anything the spawn queued behind its reply.
    connection
        .send(&FrameKind::Ping { nonce: FENCE_NONCE })
        .await
        .expect("send PING fence");
    loop {
        let frame = recv_step(connection).await;
        assert_ne!(
            content_terminal(&frame),
            Some(&spawned),
            "spawned pane content reached the client before STREAM_BIND: {frame:?}"
        );
        if matches!(frame, FrameKind::Pong { nonce: FENCE_NONCE }) {
            return spawned;
        }
    }
}

async fn wait_for_output(connection: &mut Connection, terminal: &ResourceId, needle: &[u8]) {
    let mut began = false;
    let mut tail = Vec::new();
    let deadline = Instant::now() + STEP_DEADLINE;
    while Instant::now() < deadline {
        match recv_step(connection).await {
            FrameKind::BootstrapBegin { terminal_id, .. } if &terminal_id == terminal => {
                began = true;
            }
            FrameKind::ResourceOutput {
                terminal_id, bytes, ..
            } if &terminal_id == terminal => {
                tail.extend_from_slice(&bytes);
                if tail.windows(needle.len()).any(|part| part == needle) {
                    assert!(began, "output arrived without a bound generation");
                    return;
                }
            }
            _ => {}
        }
    }
    panic!("timed out waiting for {needle:?}; tail={tail:?}");
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "shutdown consumes the connection at its last use"
)]
async fn prove_spawned_pane_binds() {
    let tmp = TempDir::new().unwrap();
    let cert = tmp.path().join("cert.pem");
    let key = tmp.path().join("key.pem");
    let mut config = seeded_config(tmp.path().join("phux.sock"), SESSION);
    config.env = quic_env(&cert, &key, None);
    let server = Server::start(config).await;
    let mut connection = dial(server.quic_addr).await;
    assert!(
        connection.multistream_enabled(),
        "QUIC_STREAMS must be negotiated"
    );

    attach_and_bind(&mut connection).await;
    let spawned = spawn_unbound_pane(&mut connection).await;

    connection
        .bind_terminal(&spawned)
        .await
        .expect("bind spawned pane");
    // The frame whose missing binding used to end the attach.
    connection
        .send(&FrameKind::ResizeTerminal {
            terminal_id: spawned.clone(),
            cols: 100,
            rows: 30,
            cell_px: None,
        })
        .await
        .expect("resize the spawned pane on its stream");
    connection
        .send(&FrameKind::InputPaste {
            terminal_id: spawned.clone(),
            event: PasteEvent {
                trust: PasteTrust::Trusted,
                data: b"hello\n".to_vec(),
            },
        })
        .await
        .expect("type into the spawned pane");
    wait_for_output(&mut connection, &spawned, b"got:hello").await;

    connection.shutdown().await;
    server.stop().await;
}

#[test]
fn quic_spawned_pane_publishes_on_its_bound_stream() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async {
        timeout(Duration::from_secs(30), prove_spawned_pane_binds())
            .await
            .expect("real QUIC spawn acceptance timed out");
    });
}
