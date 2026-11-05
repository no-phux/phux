//! The production client loop must guard and route before selecting an app.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;
use std::time::Duration;

use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::scope::TerminalScopeSet;
use phux_protocol::wire::frame::{FrameKind, HistoryTombstoneReason};
use tokio::task::LocalSet;
use tokio_util::sync::CancellationToken;

use crate::auth::AuthenticatedCredential;
use crate::hub::relay::{HubRelays, RelayHandle, RelayRequest};
use crate::policy::{ConnectionGrant, GrantFuture, PolicyEngine};
use crate::runtime::connections::dispatch::handle_client;
use crate::runtime::test_support::{Feed, Wire, encode, hello, owner_peer};
use crate::state::{ClientId, SharedState};

const CLIENT: ClientId = ClientId(21);

#[derive(Debug)]
struct FixedGrant(TerminalScopeSet);

impl PolicyEngine for FixedGrant {
    fn authorize_hello<'a>(
        &'a self,
        _peer: &'a PeerIdentity,
        _credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        let grant = ConnectionGrant::scoped(self.0.clone(), None).unwrap();
        Box::pin(async move { Ok(grant) })
    }
}

fn frames(id: ResourceId) -> Vec<FrameKind> {
    vec![
        FrameKind::InputPaste {
            terminal_id: id.clone(),
            event: PasteEvent {
                trust: PasteTrust::Trusted,
                data: b"payload".to_vec(),
            },
        },
        FrameKind::ResizeTerminal {
            terminal_id: id.clone(),
            cols: 91,
            rows: 33,
            cell_px: Some((0, 18)),
        },
        FrameKind::FrameAck {
            terminal_id: id.clone(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(2).unwrap(),
            seq: 3,
        },
        FrameKind::HistoryRequest {
            terminal_id: id.clone(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(2).unwrap(),
            cursor: bytes::Bytes::from_static(b"cursor"),
            max_bytes: 1024,
            max_rows: 10,
        },
        FrameKind::InputTerminalReply {
            terminal_id: id,
            bytes: bytes::Bytes::from_static(b"reply"),
        },
    ]
}

/// PONG is a dispatch fence: every preceding relay enqueue already happened.
async fn exchange(state: &SharedState, hello: FrameKind, frames: Vec<FrameKind>) -> Vec<FrameKind> {
    let wire = Wire::default();
    let token = CancellationToken::new();
    let mut script = vec![hello];
    script.extend(frames);
    script.push(FrameKind::Ping { nonce: 99 });
    let task = tokio::task::spawn_local(handle_client(
        Feed::scripted(script.iter().map(encode).collect()),
        wire.clone(),
        state.clone(),
        CLIENT,
        token.clone(),
        token.clone(),
        None,
        TransportType::UnixSocket,
        false,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while !wire
            .frames
            .borrow()
            .iter()
            .any(|frame| matches!(frame, FrameKind::Pong { nonce: 99 }))
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("client loop reaches PONG");
    let output = wire.frames.borrow().clone();
    token.cancel();
    task.await.unwrap().unwrap();
    output
}

#[tokio::test(flavor = "current_thread")]
async fn satellite_frames_keep_scope_proxy_and_lease_gates_before_relay() {
    LocalSet::new()
        .run_until(async {
            // Resize and ACK never acquire the input proxy/lease requirements.
            for (scope, proxy, lease_holder, input_forwarded) in [
                ("*@host:sat", false, None, false),
                ("*@host:sat", false, Some(CLIENT), false),
                ("*@host:sat", true, None, true),
                ("*@host:sat", true, Some(CLIENT), true),
                ("*@host:sat", true, Some(ClientId(22)), false),
                ("*@host:other", true, None, false),
            ] {
                let state = SharedState::new();
                let host = phux_protocol::ids::SatelliteHost::from("sat");
                let (handle, mut mailbox) = RelayHandle::new(host.clone());
                let relays = HubRelays::default();
                relays.insert(handle);
                state.with_mut(|s| {
                    s.set_peer_identity(CLIENT, owner_peer());
                    s.set_policy_engine(Arc::new(FixedGrant(
                        TerminalScopeSet::parse_all(&[scope.to_owned()]).unwrap(),
                    )));
                    s.set_hub_relays(relays);
                    if proxy {
                        s.register_satellite_proxy_attach(CLIENT, host.clone(), 7);
                    }
                    if let Some(holder) = lease_holder {
                        let (out, _rx) = tokio::sync::mpsc::channel(8);
                        s.set_satellite_lease(host.clone(), 7, holder, out);
                    }
                });
                let output = exchange(
                    &state,
                    hello("resource-frame-host"),
                    frames(ResourceId::satellite(host, 7)),
                )
                .await;
                let forwarded = forwarded_frames(&mut mailbox, lease_holder);
                let mut expected: Vec<_> =
                    frames(ResourceId::local(7)).into_iter().take(3).collect();
                if !input_forwarded {
                    expected.remove(0);
                }
                // Normalization still happens before the satellite receives resize.
                if let Some(FrameKind::ResizeTerminal { cell_px, .. }) = expected
                    .iter_mut()
                    .find(|frame| matches!(frame, FrameKind::ResizeTerminal { .. }))
                {
                    *cell_px = None;
                }
                if scope == "*@host:other" {
                    expected.clear();
                }
                assert_eq!(
                    forwarded, expected,
                    "{scope}, proxy={proxy}, holder={lease_holder:?}"
                );
                if scope == "*@host:sat" {
                    assert!(output.iter().any(|frame| matches!(
                        frame,
                        FrameKind::HistoryTombstone {
                            reason: HistoryTombstoneReason::CodecFailure,
                            ..
                        }
                    )));
                    assert!(
                        !output
                            .iter()
                            .any(|frame| matches!(frame, FrameKind::Error { .. })),
                        "{output:?}"
                    );
                }
            }
        })
        .await;
}

fn forwarded_frames(
    mailbox: &mut crate::hub::relay::RelayMailbox,
    lease_holder: Option<ClientId>,
) -> Vec<FrameKind> {
    let mut forwarded = Vec::new();
    while let Ok(request) = mailbox.requests.try_recv() {
        match request {
            RelayRequest::Forward { frame } => forwarded.push(frame),
            RelayRequest::Command {
                command: phux_protocol::wire::frame::Command::ReleaseInput { .. },
                ..
            } => assert_eq!(
                lease_holder,
                Some(CLIENT),
                "teardown releases the client's own lease"
            ),
            other => panic!("unexpected relay request: {other:?}"),
        }
    }
    forwarded
}

#[tokio::test(flavor = "current_thread")]
async fn unavailable_history_preserves_the_negotiated_codec_first_outcome() {
    use phux_protocol::caps::{
        BootstrapCapabilities, ClientCapabilities, EngineCodec, EngineFeatureSet,
    };
    LocalSet::new()
        .run_until(async {
            for (caps, reason) in [
                (
                    ClientCapabilities::new(),
                    HistoryTombstoneReason::CodecFailure,
                ),
                (
                    ClientCapabilities::new().with_bootstrap(
                        BootstrapCapabilities::new().with_native(
                            EngineCodec::LibghosttySnapshotV1,
                            EngineFeatureSet::required_native(),
                        ),
                    ),
                    HistoryTombstoneReason::Released,
                ),
            ] {
                let state = SharedState::new();
                state.with_mut(|s| s.set_peer_identity(CLIENT, owner_peer()));
                let mut hello = hello("history-no-app");
                if let FrameKind::Hello { client_caps, .. } = &mut hello {
                    *client_caps = caps;
                }
                for id in [
                    ResourceId::local(999),
                    ResourceId::satellite("offline", 999),
                ] {
                    let history = frames(id.clone()).remove(3);
                    let output = exchange(&state, hello.clone(), vec![history]).await;
                    assert!(
                        output.iter().any(|frame| matches!(frame,
                            FrameKind::HistoryTombstone { terminal_id, reason: got, cursor, .. }
                            if *terminal_id == id && *got == reason && cursor.as_ref() == b"cursor"
                        )),
                        "{output:?}"
                    );
                }
            }
        })
        .await;
}
