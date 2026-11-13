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
const DESKTOP: ClientId = ClientId(20);

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
    exchange_inspecting(state, hello, frames, || {}).await
}

/// Inspect live connection state after the dispatch fence, before cancellation
/// detaches the client and withdraws its viewport vote.
async fn exchange_inspecting(
    state: &SharedState,
    hello: FrameKind,
    frames: Vec<FrameKind>,
    inspect: impl FnOnce(),
) -> Vec<FrameKind> {
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
    inspect();
    token.cancel();
    task.await.unwrap().unwrap();
    output
}

#[tokio::test(flavor = "current_thread")]
async fn local_focus_updates_activity_before_delivery_admission_but_after_authorization() {
    LocalSet::new()
        .run_until(async {
            for (leased, retained, authorized) in [
                (true, false, true),
                (false, false, true),
                (false, true, true),
                (true, false, false),
            ] {
                local_focus_activity_case(leased, retained, authorized).await;
            }
        })
        .await;
}

async fn local_focus_activity_case(leased: bool, retained: bool, authorized: bool) {
    use crate::runtime::apps::terminal::test_support::{TerminalResource as _, stub_terminal};
    use phux_protocol::input::focus::FocusEvent;
    use phux_protocol::wire::frame::ViewportInfo;

    let state = SharedState::new();
    let (handle, mut received) = stub_terminal();
    let input = handle.terminal().unwrap().input.clone();
    let capacity = input.capacity();
    let (pane, wire, desktop_activity) = state.with_mut(|s| {
        let (_, pane) = s.seed_session("active-views");
        let wire = s
            .register_resource_handle(pane, handle, CancellationToken::new())
            .unwrap();
        for client in [DESKTOP, CLIENT] {
            s.set_peer_identity(client, owner_peer());
            let (out, _rx) = tokio::sync::mpsc::channel(8);
            s.attach_default_caps(client, "active-views", out).unwrap();
        }
        s.set_window_size(phux_config::WindowSize::Active);
        s.set_client_viewport(CLIENT, ViewportInfo::new(60, 20));
        s.set_client_viewport(DESKTOP, ViewportInfo::new(120, 48));
        s.registry_mut().terminal_mut(pane).unwrap().dims = (120, 48);
        configure_focus_admission(s, pane, leased, retained, authorized);
        (pane, wire, s.attached()[&DESKTOP].active_seq)
    });
    let focus = FrameKind::InputFocus {
        terminal_id: wire,
        event: FocusEvent::Gained,
    };
    exchange_inspecting(&state, hello("phone-focus"), vec![focus], || {
        state.with(|s| {
            assert_eq!(
                s.attached()[&CLIENT].active_seq > desktop_activity,
                authorized
            );
            let expected = if authorized && !retained {
                (60, 20)
            } else {
                (120, 48)
            };
            assert_eq!(s.registry().terminal(pane).unwrap().dims, expected);
            assert_eq!(s.input_lease_holder(pane), leased.then_some(DESKTOP));
        });
        let delivered = authorized && !leased && !retained;
        assert_eq!(
            input.capacity(),
            capacity - usize::from(delivered),
            "delivery admission remains independent of activity"
        );
        if authorized && !retained {
            let resize = received
                .resize
                .try_recv()
                .expect("phone becomes geometry authority");
            assert_eq!((resize.cols, resize.rows), (60, 20));
        }
        assert!(
            received.resize.try_recv().is_err(),
            "at most one activity resize"
        );
    })
    .await;
}

fn configure_focus_admission(
    s: &mut crate::state::ServerState,
    pane: phux_core::ids::ResourceId,
    leased: bool,
    retained: bool,
    authorized: bool,
) {
    if leased {
        s.set_input_lease(pane, DESKTOP);
    }
    if retained {
        s.restore_retained_exit(pane, phux_protocol::wire::info::ExitFacet::new(0, 1));
    }
    if !authorized {
        s.set_policy_engine(Arc::new(FixedGrant(
            TerminalScopeSet::parse_all(&["observe@global".to_owned()]).unwrap(),
        )));
    }
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
                let state = crate::runtime::apps::terminal::test_support::terminal_state();
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

/// `CLIPBOARD_REPLY` reaches the pane only from a client with input
/// authority on it (not leased by another client, not observe-only), and
/// only for selection `c` or `p`. An admitted reply is audited by size,
/// never by text.
#[tokio::test(flavor = "current_thread")]
async fn clipboard_replies_need_input_authority_and_a_known_selection() {
    LocalSet::new()
        .run_until(async {
            for (leased, authorized, selection, delivered) in [
                (false, true, b'c', true),
                (false, true, b'p', true),
                (false, true, b's', false),
                (true, true, b'c', false),
                (false, false, b'c', false),
            ] {
                clipboard_reply_case(leased, authorized, selection, delivered).await;
            }
        })
        .await;
}

async fn clipboard_reply_case(leased: bool, authorized: bool, selection: u8, delivered: bool) {
    use crate::runtime::apps::terminal::test_support::{TerminalResource as _, stub_terminal};

    let dir = tempfile::tempdir().unwrap();
    crate::audit::capture_for_test(dir.path(), crate::audit::ROTATE_BYTES);
    let state = SharedState::new();
    let (handle, _received) = stub_terminal();
    let input = handle.terminal().unwrap().input.clone();
    let capacity = input.capacity();
    let wire = state.with_mut(|s| {
        let (_, pane) = s.seed_session("clipboard");
        let wire = s
            .register_resource_handle(pane, handle, CancellationToken::new())
            .unwrap();
        for client in [DESKTOP, CLIENT] {
            s.set_peer_identity(client, owner_peer());
            let (out, _rx) = tokio::sync::mpsc::channel(8);
            s.attach_default_caps(client, "clipboard", out).unwrap();
        }
        configure_focus_admission(s, pane, leased, false, authorized);
        wire
    });
    let reply = FrameKind::ClipboardReply {
        terminal_id: wire,
        selection,
        text: phux_protocol::input::paste::ClipboardData("hunter2".to_owned()),
    };
    exchange_inspecting(&state, hello("clipboard"), vec![reply], || {
        assert_eq!(
            input.capacity(),
            capacity - usize::from(delivered),
            "leased={leased} authorized={authorized} selection={selection}"
        );
    })
    .await;
    crate::audit::release_test_capture();
    let log = std::fs::read_to_string(dir.path().join("audit.jsonl")).unwrap_or_default();
    assert_eq!(log.contains("\"clipboard.read\""), delivered, "{log}");
    assert!(!log.contains("hunter2"), "the text never reaches the log");
}
