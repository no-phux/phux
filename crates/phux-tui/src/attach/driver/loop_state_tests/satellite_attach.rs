/// Every `KILL_RESOURCE` the client sent before a FIFO barrier, with its
/// request id.
async fn kills_sent(client: &mut Connection, server: &mut Connection) -> Vec<(u32, ResourceId)> {
    kill_commands_sent(client, server)
        .await
        .into_iter()
        .filter_map(|(request_id, command)| match command {
            Command::KillResource { terminal_id, .. } => Some((request_id, terminal_id)),
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
    frames_sent(client, server)
        .await
        .into_iter()
        .filter_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: command @ (Command::KillResource { .. } | Command::KillResourceIf { .. }),
            } => Some((request_id, command)),
            _ => None,
        })
        .collect()
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
    assert_eq!(state.mirror.workspace.groups.len(), 1, "no window opened");
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
    assert_eq!(state.mirror.workspace.groups.len(), 1, "no window opened");
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
    assert_eq!(state.mirror.workspace.groups.len(), 2, "the window opened");
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
    frames_sent(&mut client, &mut server).await;
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
    let sent = frames_sent(&mut client, &mut server).await;
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

/// ADR-0136: a satellite terminal on an unchanged session list still
/// schedules one discovery sweep, and a second identical inventory does not.
#[tokio::test(flavor = "current_thread")]
async fn satellite_terminal_on_an_unchanged_session_list_schedules_one_sweep() {
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::wire::frame::{CommandResult, CommandValue};

    let (mut state, mut client, mut server, _) =
        bootstrapped_loop_with(ServerFeatureSet::with(&[ServerFeature::HostSessions])).await;
    frames_sent(&mut client, &mut server).await;
    state.peers.sweep_pending = false;
    let sat = ResourceId::satellite(SatelliteHost::new("edge"), 9);
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_sessions(vec![SessionInfo::new(SessionId::new(1), "test")])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(1), WindowId::new(1), 80, 24),
            ResourceInfo::new(sat.clone(), WindowId::new(1), 80, 24),
        ]);
    let result = CommandResult::OkWith(CommandValue::State(snapshot));
    state.fold_host_inventory(&result, &mut RepaintAccumulator::default());
    assert!(
        state.peers.sweep_pending,
        "a new satellite terminal requires discovery"
    );
    state.peers.sweep_pending = false;
    state.sweep_peer_layouts(&mut client).await.unwrap();
    let sent = frames_sent(&mut client, &mut server).await;
    assert!(
        sent.iter().any(|frame| matches!(
            frame,
            FrameKind::GetMetadata { scope, key, .. }
                if *scope == Scope::Resource(sat.clone())
                    && key == phux_client::agent_meta::RESOURCE_AGENT_KEY
        )),
        "satellite terminal is fetched: {sent:?}"
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

/// A down satellite pane keeps its layout slot. An inventory
/// that still cannot see the host does not attach it; one that can sends
/// `ATTACH_RESOURCE` and clears the flag. A replay refusal puts the flag
/// back without folding the leaf out.
#[tokio::test(flavor = "current_thread")]
async fn a_down_satellite_pane_keeps_its_slot_and_replays_on_return() {
    use phux_protocol::wire::frame::{CommandResult, ErrorCode};
    use phux_protocol::wire::info::HostInventory;

    let (mut state, mut client, mut server, _) =
        bootstrapped_loop_with(ServerFeatureSet::with(&[ServerFeature::HostSessions])).await;
    let _ = frames_sent(&mut client, &mut server).await;

    let local = ResourceId::local(1);
    let sat = ResourceId::satellite(SatelliteHost::new("devbox"), 7);
    let workspace = &mut state.mirror.workspace;
    *workspace = ArrangementDoc::single(local.clone());
    let tree = workspace
        .active_group()
        .and_then(|window| window.tree.clone())
        .expect("tree");
    workspace.active_group_mut().expect("window").tree = Some(
        crate::layout::split_at(
            &tree,
            &local,
            &sat,
            crate::layout::SplitDir::Horizontal,
            0.5,
        )
        .expect("split"),
    );
    let mut slot = PaneSlot::new().expect("slot");
    slot.satellite_down = true;
    state.mirror.panes.insert(sat.clone(), slot);

    let leaves = |state: &SessionLoop| {
        let window = state.mirror.workspace.active_group();
        crate::layout::leaves(window.and_then(|w| w.tree.as_ref()).expect("tree"))
    };

    answer_inventory(
        &mut state,
        &mut client,
        vec![HostInventory::unreachable(
            SatelliteHost::new("devbox"),
            "link is down",
        )],
    )
    .await;
    let sent = frames_sent(&mut client, &mut server).await;
    assert!(
        !sent.iter().any(|frame| matches!(
            frame,
            FrameKind::Command {
                command: Command::AttachResource { .. },
                ..
            }
        )),
        "an unreachable host is not reattached: {sent:?}"
    );
    assert!(state.mirror.panes[&sat].satellite_down);
    assert_eq!(leaves(&state), vec![local.clone(), sat.clone()]);

    answer_inventory(
        &mut state,
        &mut client,
        vec![HostInventory::reachable(
            SatelliteHost::new("devbox"),
            Vec::new(),
        )],
    )
    .await;
    let sent = frames_sent(&mut client, &mut server).await;
    let attach = sent.iter().find_map(|frame| match frame {
        FrameKind::Command {
            request_id,
            command: Command::AttachResource { terminal_id, .. },
        } if terminal_id == &sat => Some(*request_id),
        _ => None,
    });
    let attach = attach.expect("the returned host is reattached");
    assert!(!state.mirror.panes[&sat].satellite_down);
    assert_eq!(leaves(&state), vec![local.clone(), sat.clone()]);

    let mut out = Vec::new();
    state
        .apply_server_frame(
            &mut client,
            &mut out,
            None,
            FrameKind::CommandResult {
                request_id: attach,
                result: CommandResult::Error {
                    code: ErrorCode::SatelliteUnreachable,
                    message: "link is down".to_owned(),
                },
            },
            false,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    assert!(
        state.mirror.panes[&sat].satellite_down,
        "a refused replay marks the pane down again"
    );
    assert_eq!(leaves(&state), vec![local, sat]);
}
