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
    let (mut state, client, server, out) = bootstrapped_loop().await;
    state.pending_windows.insert(
        900,
        PendingWindow {
            name: "2".to_owned(),
            adopt: Some(crate::attach::actions::Adopt::Spawned(spawned_edge_pane())),
        },
    );
    (state, client, server, out)
}

/// A bootstrapped loop over a socket pair, with nothing parked.
async fn bootstrapped_loop() -> (SessionLoop, Connection, Connection, Vec<u8>) {
    let (a, b) = tokio::net::UnixStream::pair().unwrap();
    let mut client = Connection::from_stream(a);
    let server = Connection::from_stream(b);
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
    (state, client, server, out)
}

/// Every `KILL_RESOURCE` the client sent before a FIFO barrier, with its
/// request id.
async fn kills_sent(client: &mut Connection, server: &mut Connection) -> Vec<(u32, ResourceId)> {
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
                    command: Command::KillResource { terminal_id },
                } => kills.push((request_id, terminal_id)),
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
    let (mut state, _client, _server, _out) = Box::pin(loop_with_spawned_window()).await;
    let carried = state.orphans_for_switch();
    drop(state);
    let (mut next, client, server, _out) = Box::pin(bootstrapped_loop()).await;
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
