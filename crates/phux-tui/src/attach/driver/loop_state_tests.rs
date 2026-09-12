use super::*;
use phux_protocol::WindowId;
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ServerFeatureSet};
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

fn initial_attached() -> FrameKind {
    FrameKind::Attached {
        attach_id: 1,
        initial_client_id: ClientId::new(1),
        snapshot: SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
            .with_sessions(vec![SessionInfo::new(SessionId::new(1), "test")])
            .with_windows(vec![WindowInfo::new(
                WindowId::new(1),
                SessionId::new(1),
                "test",
            )])
            .with_resources(vec![ResourceInfo::new(
                ResourceId::local(1),
                WindowId::new(1),
                80,
                24,
            )]),
    }
}

async fn rename_from_prompt(state: &mut SessionLoop, conn: &mut Connection, out: &mut Vec<u8>) {
    state
        .overlays
        .push(Box::new(crate::render::overlay::PromptOverlay::new(
            "rename window",
            "rename-window",
            "name",
            "renamed",
            &state.settings.theme,
        )));
    let mut events = vec![InputEvent::Key(KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::Enter,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    })];
    state
        .dispatch_input(conn, out, None, &mut events)
        .await
        .unwrap();
}

/// A FIFO marker bounds the observation without relying on an idle-time guess.
async fn assert_no_layout_write(client: &mut Connection, server: &mut Connection) {
    client
        .send(&FrameKind::GetMetadata {
            request_id: u32::MAX,
            scope: Scope::Global,
            key: "test.barrier".into(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match server.recv().await.unwrap() {
                FrameKind::GetMetadata {
                    request_id: u32::MAX,
                    ..
                } => break,
                FrameKind::SetMetadata { .. } => panic!("layout written before initial read"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn queued_rename_cannot_write_before_initial_metadata_is_processed() {
    for metadata in [Some(b"\xa1\x67version\x02".to_vec()), None] {
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let mut client = Connection::from_stream(a);
        let mut server = Connection::from_stream(b);
        let negotiated = NegotiatedBootstrap {
            profile: BootstrapProfile::SynthesizedVtRaw,
            limits: BootstrapLimits::default(),
            server_features: ServerFeatureSet::new(),
        };
        let mut state = SessionLoop::new(
            negotiated,
            PredictiveConfig::disabled(),
            false,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let mut out = Vec::new();
        state
            .bootstrap(&mut client, &mut out, initial_attached(), None)
            .await
            .unwrap();
        let request_id = state.layout_get_request_id.unwrap();
        let before = state.workspace.clone();
        let unsupported = metadata.is_some();
        server
            .send(&FrameKind::MetadataValue {
                request_id,
                value: metadata,
            })
            .await
            .unwrap();
        // The reply is already queued on the socket, but stdin wins this turn.
        rename_from_prompt(&mut state, &mut client, &mut out).await;
        state.broadcast_layout(&mut client).await.unwrap();
        assert_no_layout_write(&mut client, &mut server).await;
        assert_eq!(state.workspace, before);
        assert!(!state.layout_read_complete);
        let reply = client.recv().await.unwrap();
        let result = state
            .apply_server_frame(
                &mut client,
                &mut out,
                None,
                reply,
                false,
                &mut RepaintAccumulator::default(),
            )
            .await;
        if unsupported {
            assert!(
                matches!(result, Err(AttachError::Protocol(message)) if message.contains("stored metadata was preserved"))
            );
            assert_eq!(state.workspace, before);
            assert!(!state.layout_read_complete);
            assert_no_layout_write(&mut client, &mut server).await;
        } else {
            result.unwrap();
            assert!(state.layout_read_complete);
            assert!(state.layout_get_request_id.is_none());
            rename_from_prompt(&mut state, &mut client, &mut out).await;
            let written = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if let FrameKind::SetMetadata { value, .. } = server.recv().await.unwrap() {
                        break Workspace::decode_cbor(&value).unwrap();
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(written.windows[0].name, "renamed");
        }
    }
}

// ---- phux-c2td.20: orphaned satellite spawns ------------------------------

/// `edge/@9`, the satellite pane a parked window spawned.
fn spawned_edge_pane() -> ResourceId {
    ResourceId::satellite(phux_protocol::ids::SatelliteHost::new("edge"), 9)
}

/// A bootstrapped loop over a socket pair, with a window parked on the
/// attach of the satellite pane it just spawned (request 900).
async fn loop_with_spawned_window() -> (SessionLoop, Connection, Connection, Vec<u8>) {
    Box::pin(loop_with_window_on(ServerFeatureSet::new(), None)).await
}

/// [`loop_with_spawned_window`] on a hub advertising `features`, the pane's
/// spawn bound to `instance` when that is `Some`.
async fn loop_with_window_on(
    features: ServerFeatureSet,
    instance: Option<phux_protocol::ids::ServerInstance>,
) -> (SessionLoop, Connection, Connection, Vec<u8>) {
    let (mut state, client, server, out) = Box::pin(bootstrapped_loop_with(features)).await;
    state.pending_windows.insert(
        900,
        PendingWindow {
            name: "2".to_owned(),
            adopt: Some(crate::attach::actions::Adopt::Spawned(
                crate::attach::actions::SpawnedPane {
                    id: spawned_edge_pane(),
                    instance,
                },
            )),
        },
    );
    (state, client, server, out)
}

/// A bootstrapped loop over a socket pair on a server advertising
/// `features`, with nothing parked.
async fn bootstrapped_loop_with(
    features: ServerFeatureSet,
) -> (SessionLoop, Connection, Connection, Vec<u8>) {
    let (a, b) = tokio::net::UnixStream::pair().unwrap();
    let mut client = Connection::from_stream(a);
    let server = Connection::from_stream(b);
    let negotiated = NegotiatedBootstrap {
        profile: BootstrapProfile::SynthesizedVtRaw,
        limits: BootstrapLimits::default(),
        server_features: features,
    };
    let mut state = SessionLoop::new(
        negotiated,
        PredictiveConfig::disabled(),
        false,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let mut out = Vec::new();
    state
        .bootstrap(&mut client, &mut out, initial_attached(), None)
        .await
        .unwrap();
    (state, client, server, out)
}

/// Drain sent frames through a FIFO barrier, with no timing-based idle guess.
async fn sidebar_frames_sent(client: &mut Connection, server: &mut Connection) -> Vec<FrameKind> {
    client
        .send(&FrameKind::GetMetadata {
            request_id: u32::MAX,
            scope: Scope::Global,
            key: "test.barrier".into(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut frames = Vec::new();
        loop {
            let frame = server.recv().await.unwrap();
            if matches!(
                frame,
                FrameKind::GetMetadata {
                    request_id: u32::MAX,
                    ..
                }
            ) {
                return frames;
            }
            frames.push(frame);
        }
    })
    .await
    .unwrap()
}

fn painted_sidebar_text(bytes: &[u8]) -> String {
    use crate::attach::render::ReplicaWalk;
    let mut probe = PaneSlot::new_with_size(100, 24).unwrap();
    probe.terminal.vt_write(bytes);
    let mut frame = phux_core::screen::RenderedFrame::blank(100, 24);
    probe
        .renderer
        .render_at_cells(
            ReplicaWalk::for_test(&probe.terminal),
            &mut frame,
            (0, 0),
            (100, 24),
        )
        .unwrap();
    frame
        .cells
        .chunks(100)
        .map(|row| {
            row[..32]
                .iter()
                .map(|c| c.grapheme.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(flavor = "current_thread")]
async fn serving_host_read_is_feature_gated_and_sent_once() {
    use phux_protocol::wire::frame::WHOAMI_KEY;
    for supported in [false, true] {
        let features = if supported {
            ServerFeatureSet::with(&[ServerFeature::Whoami])
        } else {
            ServerFeatureSet::new()
        };
        let (mut state, mut client, mut server, _) = bootstrapped_loop_with(features).await;
        state.request_serving_host(&mut client).await.unwrap();
        state.request_serving_host(&mut client).await.unwrap();
        let frames = sidebar_frames_sent(&mut client, &mut server).await;
        assert_eq!(
            frames
                .iter()
                .filter(|f| matches!(f,
                    FrameKind::GetMetadata { scope: Scope::Global, key, .. } if key == WHOAMI_KEY
                ))
                .count(),
            usize::from(supported)
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn missing_or_refused_host_identity_keeps_fallback_without_retrying() {
    for value in [
        None,
        Some(b"bad json".to_vec()),
        Some(br#"{"schema_version":2}"#.to_vec()),
    ] {
        let (mut state, mut client, mut server, _) =
            bootstrapped_loop_with(ServerFeatureSet::with(&[ServerFeature::Whoami])).await;
        state.request_serving_host(&mut client).await.unwrap();
        let request_id = state.peers.serving_host_pending.unwrap();
        state
            .intercept_peer_reply(
                &mut client,
                FrameKind::MetadataValue { request_id, value },
                &mut RepaintAccumulator::default(),
            )
            .await
            .unwrap();
        assert!(state.peers.serving_host_pending.is_none());
        assert!(state.peers.serving_host.is_none());
        sidebar_frames_sent(&mut client, &mut server).await;
        state.request_serving_host(&mut client).await.unwrap();
        assert!(
            sidebar_frames_sent(&mut client, &mut server)
                .await
                .is_empty()
        );
    }
    let (mut state, mut client, _server, _) =
        bootstrapped_loop_with(ServerFeatureSet::with(&[ServerFeature::Whoami])).await;
    state.request_serving_host(&mut client).await.unwrap();
    let request_id = state.peers.serving_host_pending.unwrap();
    let result = state
        .intercept_peer_reply(
            &mut client,
            FrameKind::Error {
                request_id: Some(request_id),
                code: phux_protocol::wire::frame::ErrorCode::InvalidCommand,
                message: "refused".into(),
            },
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    assert!(result.is_none());
    assert!(state.peers.serving_host_pending.is_none());
}

fn seed_cached_peer(state: &mut SessionLoop, peer_id: &ResourceId) {
    // ATTACHED can seed mirror slots for resources outside this workspace.
    // Such a slot must not turn a peer metadata broadcast into a local update.
    state
        .panes
        .insert(peer_id.clone(), PaneSlot::new_with_size(100, 24).unwrap());
    state
        .peers
        .sessions
        .push(SessionInfo::new(SessionId::new(2), "peer"));
    state
        .peers
        .foreign_layouts
        .insert(SessionId::new(2), Workspace::single(peer_id.clone()));
}

#[tokio::test(flavor = "current_thread")]
async fn peer_metadata_and_hostname_reach_visible_sidebar_at_the_burst_drain() {
    let (mut state, mut client, _server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    state.viewport_dims = (100, 24);
    let sidebar = Some(SidebarReservation {
        edge: crate::attach::paint::SidebarEdge::Left,
        width: 32,
    });
    let peer_id = ResourceId::local(10);
    seed_cached_peer(&mut state, &peer_id);
    state
        .peers
        .foreign_agent_pending
        .insert(900, peer_id.clone());
    state.peers.serving_host_pending = Some(901);
    let mut repaint = RepaintAccumulator::default();
    let record = AgentRecord {
        name: "reviewer".into(),
        state: phux_client::agent_meta::AgentMetaState::Working,
        ..AgentRecord::default()
    };
    state
        .intercept_peer_reply(
            &mut client,
            FrameKind::MetadataValue {
                request_id: 900,
                value: Some(record.encode()),
            },
            &mut repaint,
        )
        .await
        .unwrap();
    let whoami = br#"{"schema_version":1,"principal":null,"credential_id":null,"auth_route":"uds","peer_uid":501,"serving_user":{"uid":501,"name":"test"},"host":"remote-mini","server_version":"test"}"#;
    state
        .intercept_peer_reply(
            &mut client,
            FrameKind::MetadataValue {
                request_id: 901,
                value: Some(whoami.to_vec()),
            },
            &mut repaint,
        )
        .await
        .unwrap();
    assert!(!state.overlays.is_active());
    out.clear();
    state.drain_repaint(&mut out, sidebar, &mut repaint);
    let screen = painted_sidebar_text(&out);
    assert!(screen.contains("reviewer"), "{screen}");
    assert!(screen.contains("remote-mini"), "{screen}");
    assert!(
        !out.windows(4).any(|s| s == b"\x1b[2J"),
        "metadata must not clear the screen"
    );
    let first_targets = state.sidebar_painter.click_targets().needs_you;

    let done = AgentRecord {
        state: phux_client::agent_meta::AgentMetaState::Done,
        ..record
    };
    let frame = FrameKind::MetadataChanged {
        scope: Scope::Resource(peer_id),
        key: phux_client::agent_meta::RESOURCE_AGENT_KEY.to_owned(),
        value: Some(done.encode()),
    };
    out.clear();
    state
        .apply_server_frame(
            &mut client,
            &mut out,
            sidebar,
            frame.clone(),
            true,
            &mut repaint,
        )
        .await
        .unwrap();
    state.drain_repaint(&mut out, sidebar, &mut repaint);
    assert!(
        !out.is_empty(),
        "broadcast must repaint without another input"
    );
    assert!(painted_sidebar_text(&out).contains("reviewer"));
    assert_eq!(
        state.sidebar_painter.click_targets().needs_you,
        first_targets
    );
    out.clear();
    state
        .apply_server_frame(&mut client, &mut out, sidebar, frame, true, &mut repaint)
        .await
        .unwrap();
    state.drain_repaint(&mut out, sidebar, &mut repaint);
    assert!(out.is_empty(), "identical metadata must emit no bytes");
}

#[tokio::test(flavor = "current_thread")]
async fn peer_layout_broadcast_discovers_and_subscribes_new_agent_leaves() {
    let (mut state, mut client, mut server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    sidebar_frames_sent(&mut client, &mut server).await;
    let id = ResourceId::local(10);
    let frame = FrameKind::MetadataChanged {
        scope: Scope::Group(phux_client::layout_ops::DEFAULT_LAYOUT_GROUP_ID),
        key: phux_client::layout_ops::layout_key(SessionId::new(2)),
        value: Some(Workspace::single(id.clone()).encode_cbor().unwrap()),
    };
    state
        .apply_server_frame(
            &mut client,
            &mut out,
            None,
            frame,
            true,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    let sent = sidebar_frames_sent(&mut client, &mut server).await;
    assert!(sent.iter().any(|f| matches!(f, FrameKind::GetMetadata { scope: Scope::Resource(r), key, .. } if r == &id && key == phux_client::agent_meta::RESOURCE_AGENT_KEY)));
    assert!(sent.iter().any(|f| matches!(f, FrameKind::SubscribeMetadata { scope: Scope::Resource(r), key } if r == &id && key == phux_client::agent_meta::RESOURCE_AGENT_KEY)));
}

/// Every `KILL_RESOURCE` the client sent before a FIFO barrier, with its
/// request id.
async fn kills_sent(client: &mut Connection, server: &mut Connection) -> Vec<(u32, ResourceId)> {
    kill_commands_sent(client, server)
        .await
        .into_iter()
        .filter_map(|(request_id, command)| match command {
            Command::KillResource { terminal_id } => Some((request_id, terminal_id)),
            _ => None,
        })
        .collect()
}

/// Every kill the client sent before a FIFO barrier, `KILL_RESOURCE` and
/// `KILL_RESOURCE_IF` alike, with its request id.
async fn kill_commands_sent(
    client: &mut Connection,
    server: &mut Connection,
) -> Vec<(u32, Command)> {
    client
        .send(&FrameKind::GetMetadata {
            request_id: u32::MAX,
            scope: Scope::Global,
            key: "test.barrier".into(),
        })
        .await
        .unwrap();
    let mut kills = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match server.recv().await.unwrap() {
                FrameKind::GetMetadata {
                    request_id: u32::MAX,
                    ..
                } => break,
                FrameKind::Command {
                    request_id,
                    command:
                        command @ (Command::KillResource { .. } | Command::KillResourceIf { .. }),
                } => kills.push((request_id, command)),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    kills
}

/// Feed the parked spawned window's attach a refusal with `code`.
async fn refuse_spawned_attach(
    state: &mut SessionLoop,
    client: &mut Connection,
    out: &mut Vec<u8>,
    code: phux_protocol::wire::frame::ErrorCode,
) {
    state
        .apply_server_frame(
            client,
            out,
            None,
            FrameKind::CommandResult {
                request_id: 900,
                result: phux_protocol::wire::frame::CommandResult::Error {
                    code,
                    message: "attach refused".to_owned(),
                },
            },
            false,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
}

/// A refused attach of a spawned satellite window sends exactly one kill,
/// for that pane on that host, and the kill's own refusal is consumed
/// without reaching the frame handler.
#[tokio::test(flavor = "current_thread")]
async fn a_refused_spawned_attach_sends_one_kill_for_its_pane() {
    use phux_protocol::wire::frame::ErrorCode;

    let (mut state, mut client, mut server, mut out) = loop_with_spawned_window().await;
    refuse_spawned_attach(
        &mut state,
        &mut client,
        &mut out,
        ErrorCode::TerminalNotFound,
    )
    .await;

    let kills = kills_sent(&mut client, &mut server).await;
    assert_eq!(kills.len(), 1, "{kills:?}");
    let (kill_id, pane) = &kills[0];
    assert_eq!(*pane, spawned_edge_pane());
    assert_eq!(
        pane.host().map(phux_protocol::ids::SatelliteHost::as_str),
        Some("edge")
    );
    assert_eq!(state.workspace.windows.len(), 1, "no window opened");
    let refused_kill = FrameKind::Error {
        request_id: Some(*kill_id),
        code: ErrorCode::SatelliteUnreachable,
        message: "satellite edge link is down".to_owned(),
    };
    assert_eq!(state.orphan_kills.observe(refused_kill), None);
}

/// A refusal saying the satellite is unreachable sends no kill: none could
/// reach the pane, and the hub would hold this client's next frames behind
/// the attempt for up to its 30 s relay deadline.
#[tokio::test(flavor = "current_thread")]
async fn an_unreachable_satellite_refusal_sends_no_kill() {
    use phux_protocol::wire::frame::ErrorCode;

    let (mut state, mut client, mut server, mut out) = loop_with_spawned_window().await;
    refuse_spawned_attach(
        &mut state,
        &mut client,
        &mut out,
        ErrorCode::SatelliteUnreachable,
    )
    .await;

    assert_eq!(kills_sent(&mut client, &mut server).await, Vec::new());
    assert_eq!(state.workspace.windows.len(), 1, "no window opened");
}

/// A successful attach opens the window and kills nothing.
#[tokio::test(flavor = "current_thread")]
async fn a_successful_spawned_attach_sends_no_kill() {
    use phux_protocol::wire::frame::CommandResult;

    let (mut state, mut client, mut server, mut out) = loop_with_spawned_window().await;
    state
        .apply_server_frame(
            &mut client,
            &mut out,
            None,
            FrameKind::CommandResult {
                request_id: 900,
                result: CommandResult::Ok,
            },
            false,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();

    assert_eq!(kills_sent(&mut client, &mut server).await, Vec::new());
    assert_eq!(state.workspace.windows.len(), 2, "the window opened");
}

// ---- phux-c2td.23: retried kills for stray satellite panes ---------------

fn edge_row(reachable: bool) -> phux_protocol::wire::info::HostInventory {
    use phux_protocol::wire::info::HostInventory;
    if reachable {
        HostInventory::reachable(SatelliteHost::new("edge"), Vec::new())
    } else {
        HostInventory::unreachable(SatelliteHost::new("edge"), "link is down")
    }
}

/// A new inventory schedules discovery; an identical reply terminates the sweep.
#[tokio::test(flavor = "current_thread")]
async fn fresh_inventory_discovers_peer_sessions_without_an_endless_sweep() {
    use phux_protocol::wire::frame::{CommandResult, CommandValue};
    let (mut state, mut client, mut server, _) =
        bootstrapped_loop_with(ServerFeatureSet::with(&[ServerFeature::HostSessions])).await;
    sidebar_frames_sent(&mut client, &mut server).await;
    state.peers.sweep_pending = false;
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "test"),
            SessionInfo::new(SessionId::new(2), "new-peer"),
        ]);
    let result = CommandResult::OkWith(CommandValue::State(snapshot));
    state.fold_host_inventory(&result, &mut RepaintAccumulator::default());
    assert!(
        state.peers.sweep_pending,
        "new graph requires leaf discovery"
    );
    state.peers.sweep_pending = false;
    state.sweep_peer_layouts(&mut client).await.unwrap();
    let sent = sidebar_frames_sent(&mut client, &mut server).await;
    assert!(
        sent.iter()
            .any(|f| matches!(f, FrameKind::GetMetadata { key, .. }
        if key == &phux_client::layout_ops::layout_key(SessionId::new(2))))
    );
    state.fold_host_inventory(&result, &mut RepaintAccumulator::default());
    assert!(
        !state.peers.sweep_pending,
        "identical inventory must not loop"
    );
}

/// Answer a host-inventory `GET_STATE` asked just now with `rows`.
async fn answer_inventory(
    state: &mut SessionLoop,
    client: &mut Connection,
    rows: Vec<phux_protocol::wire::info::HostInventory>,
) {
    use phux_protocol::wire::frame::{CommandResult, CommandValue};

    state.peers.hosts_pending = Some(777);
    state.peers.hosts_pending_since = Some(std::time::Instant::now());
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_hosts(rows);
    let passed = state
        .intercept_peer_reply(
            client,
            FrameKind::CommandResult {
                request_id: 777,
                result: CommandResult::OkWith(CommandValue::State(snapshot)),
            },
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    assert!(passed.is_none(), "the inventory reply is consumed");
}

/// The panes of every `KILL_RESOURCE` sent before a FIFO barrier.
async fn killed_panes(client: &mut Connection, server: &mut Connection) -> Vec<ResourceId> {
    kills_sent(client, server)
        .await
        .into_iter()
        .map(|(_, pane)| pane)
        .collect()
}

/// The next loop entry after a session switch that dropped a window still
/// waiting on the attach of `edge/@9`, the pane it spawned: that pane is a
/// stray carried into this loop.
async fn loop_with_switch_stray() -> (SessionLoop, Connection, Connection) {
    Box::pin(loop_with_switch_stray_on(ServerFeatureSet::new, None)).await
}

/// [`loop_with_switch_stray`] on a hub advertising `features`, the stray's
/// spawn bound to `instance` when that is `Some`.
async fn loop_with_switch_stray_on(
    features: fn() -> ServerFeatureSet,
    instance: Option<phux_protocol::ids::ServerInstance>,
) -> (SessionLoop, Connection, Connection) {
    let (mut state, _client, _server, _out) =
        Box::pin(loop_with_window_on(features(), instance)).await;
    let carried = state.orphans_for_switch();
    drop(state);
    let (mut next, client, server, _out) = Box::pin(bootstrapped_loop_with(features())).await;
    next.set_orphan_kills(carried);
    (next, client, server)
}

/// A refusal saying the satellite is unreachable records nothing: neither
/// a reachable inventory nor a spawn on that satellite later kills the pane,
/// since the satellite may have restarted and reused its id.
#[tokio::test(flavor = "current_thread")]
async fn an_unreachable_refusal_records_nothing() {
    use phux_protocol::wire::frame::ErrorCode;

    let (mut state, mut client, mut server, mut out) = Box::pin(loop_with_spawned_window()).await;
    Box::pin(refuse_spawned_attach(
        &mut state,
        &mut client,
        &mut out,
        ErrorCode::SatelliteUnreachable,
    ))
    .await;
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    Box::pin(answer_edge_spawn(&mut state, &mut client, 12)).await;
    assert_eq!(killed_panes(&mut client, &mut server).await, Vec::new());
}

/// A pane stranded by a session switch is killed, once, on the first reply
/// saying its satellite is reachable.
#[tokio::test(flavor = "current_thread")]
async fn a_switch_stray_is_killed_on_the_next_reachable_reply() {
    let (mut state, mut client, mut server) = Box::pin(loop_with_switch_stray()).await;
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(
        killed_panes(&mut client, &mut server).await,
        vec![spawned_edge_pane()]
    );

    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(
        killed_panes(&mut client, &mut server).await,
        Vec::new(),
        "the kill is sent once"
    );
}

/// A switch stray whose satellite answers only after
/// [`super::super::orphans::STRAY_TTL`] is forgotten, not killed.
#[tokio::test(flavor = "current_thread")]
async fn a_switch_stray_is_not_killed_after_its_ttl() {
    let (mut state, mut client, mut server) = Box::pin(loop_with_switch_stray()).await;
    state
        .orphan_kills
        .age_strays(super::super::orphans::STRAY_TTL + Duration::from_secs(1));
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(killed_panes(&mut client, &mut server).await, Vec::new());
}

/// An inventory that lists the stray's satellite as unreachable forgets it:
/// a later reachable one kills nothing.
#[tokio::test(flavor = "current_thread")]
async fn an_unreachable_inventory_row_drops_switch_strays() {
    let (mut state, mut client, mut server) = Box::pin(loop_with_switch_stray()).await;
    answer_inventory(&mut state, &mut client, vec![edge_row(false)]).await;
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(killed_panes(&mut client, &mut server).await, Vec::new());
}

/// A stray this client has since adopted, in a window or in an open
/// waiting on its attach, is not killed when its satellite answers.
#[tokio::test(flavor = "current_thread")]
async fn an_adopted_stray_is_not_killed_on_retry() {
    use crate::attach::actions::Adopt;

    for adopted_by_window in [true, false] {
        let (mut state, mut client, mut server) = Box::pin(loop_with_switch_stray()).await;
        if adopted_by_window {
            state
                .workspace
                .add_window("edge".to_owned(), spawned_edge_pane());
        } else {
            state.pending_windows.insert(
                901,
                PendingWindow {
                    name: "edge/build".to_owned(),
                    adopt: Some(Adopt::Existing(spawned_edge_pane())),
                },
            );
        }
        answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
        assert_eq!(
            killed_panes(&mut client, &mut server).await,
            Vec::new(),
            "adopted by a window: {adopted_by_window}"
        );
    }
}

/// Feed a satellite `new-window` spawn (request 901) its reply: `edge`
/// minted `edge/@{id}`.
async fn answer_edge_spawn(state: &mut SessionLoop, client: &mut Connection, id: u32) {
    use phux_protocol::wire::frame::SpawnResult;

    state.pending_windows.insert(
        901,
        PendingWindow {
            name: "3".to_owned(),
            adopt: None,
        },
    );
    let mut out = Vec::new();
    state
        .apply_server_frame(
            client,
            &mut out,
            None,
            FrameKind::ResourceSpawned {
                request_id: 901,
                result: SpawnResult::Ok(ResourceId::satellite(SatelliteHost::new("edge"), id)),
            },
            false,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
}

/// A satellite answering a relayed spawn is reachable, so its switch strays
/// are killed then too; one whose id that satellite has minted again (it
/// restarted) is forgotten instead.
#[tokio::test(flavor = "current_thread")]
async fn a_spawn_on_the_host_kills_its_strays_unless_it_restarted() {
    let (mut state, mut client, mut server) = Box::pin(loop_with_switch_stray()).await;
    Box::pin(answer_edge_spawn(&mut state, &mut client, 12)).await;
    assert_eq!(
        killed_panes(&mut client, &mut server).await,
        vec![spawned_edge_pane()]
    );

    let (mut state, mut client, mut server) = Box::pin(loop_with_switch_stray()).await;
    Box::pin(answer_edge_spawn(&mut state, &mut client, 5)).await;
    assert_eq!(killed_panes(&mut client, &mut server).await, Vec::new());
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(
        killed_panes(&mut client, &mut server).await,
        Vec::new(),
        "forgotten, not waiting"
    );
}

// ---- phux-c2td.25: conditional retries of stray satellite panes ----------

/// The instance token `edge` bound its spawns to.
fn edge_token() -> phux_protocol::ids::ServerInstance {
    phux_protocol::ids::ServerInstance::new([5; 16])
}

/// A hub that evaluates conditional kills.
fn conditional_kill() -> ServerFeatureSet {
    ServerFeatureSet::with(&[phux_protocol::caps::ServerFeature::ConditionalKill])
}

/// The conditional kill of `edge/@9` under [`edge_token`].
fn conditional_edge_kill() -> Command {
    Command::KillResourceIf {
        terminal_id: spawned_edge_pane(),
        precondition: phux_protocol::wire::frame::KillPrecondition::spawned_and_unattached(
            edge_token(),
        ),
    }
}

/// The unconditional kill of `edge/@9`.
fn plain_edge_kill() -> Command {
    Command::KillResource {
        terminal_id: spawned_edge_pane(),
    }
}

/// The kills sent before a FIFO barrier, without their request ids.
async fn kill_commands(client: &mut Connection, server: &mut Connection) -> Vec<Command> {
    kill_commands_sent(client, server)
        .await
        .into_iter()
        .map(|(_, command)| command)
        .collect()
}

/// A bound spawn stranded by an unreachable satellite is recorded, sends
/// nothing while the satellite is down, and is retried once with
/// `KILL_RESOURCE_IF` carrying its instance token once an inventory lists
/// the satellite reachable. The satellite's `PRECONDITION_FAILED` is
/// consumed and forgets the stray: no second attempt, and no unconditional
/// fallback.
#[tokio::test(flavor = "current_thread")]
async fn a_bound_unreachable_stray_is_retried_conditionally_once_reachable() {
    use phux_protocol::wire::frame::{CommandResult, ErrorCode};

    let (mut state, mut client, mut server, mut out) =
        Box::pin(loop_with_window_on(conditional_kill(), Some(edge_token()))).await;
    Box::pin(refuse_spawned_attach(
        &mut state,
        &mut client,
        &mut out,
        ErrorCode::SatelliteUnreachable,
    ))
    .await;
    assert_eq!(
        kill_commands(&mut client, &mut server).await,
        Vec::new(),
        "no kill while the satellite is unreachable"
    );

    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    let sent = kill_commands_sent(&mut client, &mut server).await;
    assert_eq!(
        sent.iter()
            .map(|(_, command)| command.clone())
            .collect::<Vec<_>>(),
        vec![conditional_edge_kill()]
    );
    let refused = FrameKind::CommandResult {
        request_id: sent[0].0,
        result: CommandResult::Error {
            code: ErrorCode::PreconditionFailed,
            message: "resource was attached by another connection".to_owned(),
        },
    };
    assert_eq!(state.orphan_kills.observe(refused), None, "consumed");

    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    Box::pin(answer_edge_spawn(&mut state, &mut client, 12)).await;
    assert_eq!(
        kill_commands(&mut client, &mut server).await,
        Vec::new(),
        "forgotten after its one retry"
    );
}

/// Without `CONDITIONAL_KILL` a bound spawn's unreachable refusal records
/// nothing, as before: neither a reachable inventory nor a spawn on the
/// satellite sends any kill, conditional or not.
#[tokio::test(flavor = "current_thread")]
async fn without_the_bit_an_unreachable_refusal_sends_no_kill_at_all() {
    use phux_protocol::wire::frame::ErrorCode;

    let (mut state, mut client, mut server, mut out) = Box::pin(loop_with_window_on(
        ServerFeatureSet::new(),
        Some(edge_token()),
    ))
    .await;
    Box::pin(refuse_spawned_attach(
        &mut state,
        &mut client,
        &mut out,
        ErrorCode::SatelliteUnreachable,
    ))
    .await;
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    Box::pin(answer_edge_spawn(&mut state, &mut client, 12)).await;
    assert_eq!(kill_commands(&mut client, &mut server).await, Vec::new());
}

/// A bound stray under the conditional kill outlives signs that its
/// satellite is unreachable (an unreachable inventory row, an uncorrelated
/// notice): the satellite judges the kill when it answers again.
#[tokio::test(flavor = "current_thread")]
async fn a_bound_stray_outlives_unreachable_signals() {
    use phux_protocol::wire::frame::ErrorCode;

    let (mut state, mut client, mut server, mut out) =
        Box::pin(loop_with_window_on(conditional_kill(), Some(edge_token()))).await;
    Box::pin(refuse_spawned_attach(
        &mut state,
        &mut client,
        &mut out,
        ErrorCode::SatelliteUnreachable,
    ))
    .await;
    answer_inventory(&mut state, &mut client, vec![edge_row(false)]).await;
    let notice = FrameKind::Error {
        request_id: None,
        code: ErrorCode::SatelliteUnreachable,
        message: "satellite edge is unreachable: link is down".to_owned(),
    };
    assert!(state.orphan_kills.observe(notice).is_some());
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(
        kill_commands(&mut client, &mut server).await,
        vec![conditional_edge_kill()]
    );
}

/// A session-switch stray whose spawn was bound is retried with the
/// conditional kill on a hub with the bit, and with today's unconditional
/// kill on one without it.
#[tokio::test(flavor = "current_thread")]
async fn a_bound_switch_stray_uses_the_conditional_kill_when_supported() {
    let cases: [(fn() -> ServerFeatureSet, Command); 2] = [
        (conditional_kill, conditional_edge_kill()),
        (ServerFeatureSet::new, plain_edge_kill()),
    ];
    for (features, expected) in cases {
        let (mut state, mut client, mut server) =
            Box::pin(loop_with_switch_stray_on(features, Some(edge_token()))).await;
        answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
        assert_eq!(
            kill_commands(&mut client, &mut server).await,
            vec![expected]
        );
    }
}

/// An unbound switch stray keeps today's behavior on a hub with the bit:
/// an unconditional kill on the next reachable reply, and forgotten by an
/// unreachable inventory row first.
#[tokio::test(flavor = "current_thread")]
async fn an_unbound_switch_stray_is_unchanged_under_the_bit() {
    let (mut state, mut client, mut server) =
        Box::pin(loop_with_switch_stray_on(conditional_kill, None)).await;
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(
        kill_commands(&mut client, &mut server).await,
        vec![plain_edge_kill()]
    );

    let (mut state, mut client, mut server) =
        Box::pin(loop_with_switch_stray_on(conditional_kill, None)).await;
    answer_inventory(&mut state, &mut client, vec![edge_row(false)]).await;
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(kill_commands(&mut client, &mut server).await, Vec::new());
}

/// A bound stray this client has since adopted is not killed, even
/// conditionally: the satellite exempts this client's own attaches.
#[tokio::test(flavor = "current_thread")]
async fn an_adopted_bound_stray_is_not_killed_conditionally() {
    let (mut state, mut client, mut server) = Box::pin(loop_with_switch_stray_on(
        conditional_kill,
        Some(edge_token()),
    ))
    .await;
    state
        .workspace
        .add_window("edge".to_owned(), spawned_edge_pane());
    answer_inventory(&mut state, &mut client, vec![edge_row(true)]).await;
    assert_eq!(kill_commands(&mut client, &mut server).await, Vec::new());
}

// ---- phux-c2td.3: held federation notices --------------------------------

fn host_rows() -> Vec<phux_protocol::wire::info::HostInventory> {
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::wire::info::HostInventory;

    vec![
        HostInventory::unreachable(
            SatelliteHost::new("down"),
            "satellite down is unreachable: link is down",
        ),
        HostInventory::reachable(SatelliteHost::new("edge"), Vec::new()),
    ]
}

/// The reply drops only the notices its unreachable rows explain; a link
/// drop for a satellite it still lists as reachable surfaces.
#[test]
fn held_notices_surface_unless_the_inventory_explains_them() {
    let held = vec![
        // Explained: the row carries exactly this diagnostic.
        "satellite down is unreachable: link is down".to_owned(),
        // Explained: same host, a different reason.
        "satellite down is unreachable: dial refused".to_owned(),
        // Not explained: the inventory lists `edge` as reachable.
        "satellite edge is unreachable: link dropped".to_owned(),
        // Not explained: a different host whose name merely extends `down`.
        "satellite downstairs is unreachable: gone".to_owned(),
    ];
    assert_eq!(
        unexplained_unreachable_notices(held, &host_rows()),
        vec![
            "satellite edge is unreachable: link dropped".to_owned(),
            "satellite downstairs is unreachable: gone".to_owned(),
        ]
    );
}

/// An inventory with no unreachable rows explains nothing, and a refusal
/// (no inventory at all) surfaces everything held.
#[test]
fn held_notices_all_surface_when_nothing_explains_them() {
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::wire::info::HostInventory;

    let rows = vec![HostInventory::reachable(
        SatelliteHost::new("down"),
        Vec::new(),
    )];
    let held = vec!["satellite down is unreachable: link is down".to_owned()];
    assert_eq!(unexplained_unreachable_notices(held.clone(), &rows), held);
    assert_eq!(unexplained_unreachable_notices(held.clone(), &[]), held);
}

/// A request is overdue only past the deadline, and never when none is in
/// flight.
#[test]
fn host_inventory_overdue_only_past_the_deadline() {
    let now = std::time::Instant::now();
    assert!(!host_inventory_overdue(None, now));
    assert!(!host_inventory_overdue(Some(now), now));
    let sent = now.checked_sub(HOST_INVENTORY_DEADLINE + std::time::Duration::from_secs(1));
    assert!(sent.is_some_and(|sent| host_inventory_overdue(Some(sent), now)));
}

/// Held notices surface in the same wording the frame handler uses for a
/// live degradation notice.
#[test]
fn federation_notices_use_the_degraded_wording() {
    let notices = federation_notices(vec!["satellite edge is unreachable: x".to_owned()]);
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].text,
        "federation degraded: satellite edge is unreachable: x"
    );
}
