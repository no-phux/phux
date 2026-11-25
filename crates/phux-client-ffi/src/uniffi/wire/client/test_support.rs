//! Shared fixtures for the client binding tests: a sans-IO remote client
//! attached to bootstrapped panes, with no socket.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "test fixtures")]

use super::*;
use phux_client_runtime::control::{ControlOptions, ControlPlane};
use phux_protocol::ids::{SessionId, WindowId};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
use phux_wire_fixtures::{HelloOk, attached, decode_one, l3_caps};

pub(super) const COLS: u16 = 24;
pub(super) const ROWS: u16 = 4;

/// Feed `bytes` to `terminal_id` as one complete bootstrap.
fn bootstrap(client: &Client, terminal_id: ResourceId, bytes: &[u8]) {
    for frame in phux_wire_fixtures::bootstrap_frames(&terminal_id, [1, 1], [COLS, ROWS], bytes) {
        client.feed(frame).expect("bootstrap frame");
    }
}

/// A `RemoteClient` over a sans-IO runtime client attached to `panes`, each
/// bootstrapped with its bytes. No socket: frames are fed directly.
pub(super) fn remote_with(panes: &[(u32, Vec<u8>)]) -> Arc<RemoteClient> {
    let client = Runtime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("main".into())),
        viewport: (COLS, ROWS),
        ..ControlOptions::default()
    });
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(
            HelloOk::new([0xAB; 16], l3_caps())
                .features(phux_protocol::caps::ServerFeatureSet::with(&[
                    phux_protocol::caps::ServerFeature::AcknowledgedInput,
                ]))
                .build(),
        )
        .expect("HELLO_OK");
    let attach_id = client
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode_one(bytes) {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .expect("ATTACH queued");
    let session = SessionId::new(1);
    let window = WindowId::new(1);
    let resources = panes
        .iter()
        .map(|(id, _)| ResourceInfo::new(ResourceId::local(*id), window, COLS, ROWS))
        .collect();
    let snapshot = SessionSnapshot::new(session, window, ResourceId::local(panes[0].0))
        .with_sessions(vec![SessionInfo::new(session, "main")])
        .with_windows(vec![WindowInfo::new(window, session, "shell")])
        .with_resources(resources);
    client
        .feed(attached(attach_id, snapshot, 1))
        .expect("ATTACHED");
    for (id, bytes) in panes {
        bootstrap(&client, ResourceId::local(*id), bytes);
    }
    client
        .feed(FrameKind::AttachReady { attach_id })
        .expect("ATTACH_READY");
    let _ = client.take_outbound();
    let remote = RemoteClient::new("unused".into(), COLS, ROWS, None, None);
    *remote.client.lock().unwrap() = Some(client);
    remote
}
