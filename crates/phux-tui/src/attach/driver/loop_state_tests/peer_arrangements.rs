/// ADR-0161: with an arrangement link, the peer sweep reads each peer's
/// arrangement record over it instead of the translated layout key, and a
/// peer's `ARRANGEMENT_CHANGED` re-reads it.
#[tokio::test(flavor = "current_thread")]
async fn peer_sweep_reads_arrangement_records_over_the_link() {
    use phux_client::testkit::{OrgFixture, ScriptSpec, serve_one};
    use phux_protocol::wire::frame::{
        ArrangementDoc as WireDoc, ArrangementGroup, ArrangementNode, EntryRef, EntryTarget,
        SessionLifetime,
    };
    let mut fixture = OrgFixture::new();
    let ours = fixture.session("test", SessionLifetime::Persistent);
    fixture.shell(ours);
    let peer = fixture.session("peer", SessionLifetime::Persistent);
    let shell = fixture.shell(peer);
    fixture.arrangement(
        peer,
        WireDoc {
            groups: vec![ArrangementGroup {
                id: [7; 16],
                name: Some("build".to_owned()),
                tree: ArrangementNode::Leaf(EntryRef {
                    entry: [1; 16],
                    target: EntryTarget::Member(shell),
                }),
            }],
        },
    );
    let our_id = fixture.live_session(&ours).unwrap();
    let peer_id = fixture.live_session(&peer).unwrap();
    let pane = fixture.live(&shell).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let spec = ScriptSpec::new()
        .server_features_ext(phux_protocol::ServerFeatureExtSet::with(&[
            phux_protocol::ServerFeatureExt::Organization,
        ]))
        .organization(fixture);
    let (socket, _org_server) = serve_one(dir.path(), spec);

    let (mut state, mut client, mut server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let _ = frames_sent(&mut client, &mut server).await;
    state.peer_reader = Some(crate::attach::peer_arrangements::PeerReader::spawn(
        crate::attach::Dial::uds(&socket),
        our_id,
    ));
    state.peers.focused_session = Some(our_id);
    state.peers.sessions = vec![
        SessionInfo::new(our_id, "test").with_key(Some(ours)),
        SessionInfo::new(peer_id, "peer").with_key(Some(peer)),
    ];
    state.sweep_peer_layouts(&mut client).await.unwrap();
    settle_peer_reads(&mut state, &mut client, &mut out).await;
    let cached = state
        .peers
        .foreign_layouts
        .get(&peer_id)
        .expect("peer read");
    assert_eq!(cached.groups[0].name, "build");
    assert_eq!(
        crate::layout::leaves(cached.groups[0].state.tree.as_ref().unwrap()),
        vec![pane]
    );
    assert!(!state.peers.foreign_layouts.contains_key(&our_id));
    let sent = frames_sent(&mut client, &mut server).await;
    assert!(
        !sent.iter().any(|frame| matches!(
            frame,
            FrameKind::GetMetadata { key, .. } | FrameKind::SubscribeMetadata { key, .. }
                if key.starts_with(phux_client::layout_ops::LAYOUT_KEY)
        )),
        "no translated layout key is read: {sent:?}"
    );

    // A peer's change re-reads only that peer.
    state.peers.foreign_layouts.clear();
    state.peers.arrangement_reads_owed.add(peer);
    settle_peer_reads(&mut state, &mut client, &mut out).await;
    assert!(state.peers.foreign_layouts.contains_key(&peer_id));
}

/// Send what the loop owes the peer reader and fold its answer, as the
/// burst drain and the reader's `select!` arm do.
async fn settle_peer_reads(state: &mut SessionLoop, client: &mut Connection, out: &mut Vec<u8>) {
    state.request_peer_arrangement_reads();
    let done = tokio::time::timeout(
        Duration::from_secs(5),
        state.peer_reader.as_mut().expect("reader").next(),
    )
    .await
    .expect("peer read answered");
    state
        .on_peer_reads_done(client, out, None, done)
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn refused_rename_barrier_keeps_the_current_status_name() {
    let (mut state, mut client, _server, _) = bootstrapped_loop_with(ServerFeatureSet::new()).await;
    parked_rename(&mut state, "notes");
    answer_rename_barrier(
        &mut state,
        &mut client,
        vec![
            SessionInfo::new(SessionId::new(1), "test"),
            SessionInfo::new(SessionId::new(2), "taken"),
        ],
    )
    .await;
    assert!(state.rename_pending.is_none());
    assert_eq!(
        state.mirror.session_name, "test",
        "a refused rename must leave the current status name authoritative"
    );
    let names: Vec<&str> = state
        .peers
        .sessions
        .iter()
        .map(|session| session.name.as_str())
        .collect();
    assert_eq!(names, ["test", "taken"]);
}

#[tokio::test(flavor = "current_thread")]
async fn confirmed_rename_barrier_applies_the_new_name() {
    let (mut state, mut client, _server, _) = bootstrapped_loop_with(ServerFeatureSet::new()).await;
    parked_rename(&mut state, "notes");
    answer_rename_barrier(
        &mut state,
        &mut client,
        vec![
            SessionInfo::new(SessionId::new(1), "notes"),
            SessionInfo::new(SessionId::new(2), "peer"),
        ],
    )
    .await;
    assert!(state.rename_pending.is_none());
    assert_eq!(state.mirror.session_name, "notes");
    let ours = state
        .peers
        .sessions
        .iter()
        .find(|session| session.id == SessionId::new(1))
        .expect("this session remains");
    assert_eq!(ours.name, "notes");
}

#[tokio::test(flavor = "current_thread")]
async fn rename_barrier_error_keeps_the_current_status_name() {
    let (mut state, mut client, _server, _) = bootstrapped_loop_with(ServerFeatureSet::new()).await;
    parked_rename(&mut state, "notes");
    let passed = state
        .intercept_peer_reply(
            &mut client,
            FrameKind::Error {
                request_id: Some(4242),
                code: phux_protocol::wire::frame::ErrorCode::InternalError,
                message: "get-state failed".to_owned(),
            },
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    assert!(passed.is_none());
    assert!(state.rename_pending.is_none());
    assert_eq!(state.mirror.session_name, "test");
}

/// A CLI-created peer with an agent record but no TUI layout
/// still paints in Agents, keyed by `ResourceId`.
#[tokio::test(flavor = "current_thread")]
async fn unvisited_peer_agent_paints_from_server_inventory() {
    let (mut state, _client, _server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    state.viewport_dims = (100, 24);
    let sidebar = Some(SidebarReservation {
        edge: crate::attach::paint::SidebarEdge::Left,
        width: 32,
    });
    let peer = ResourceId::local(10);
    state
        .peers
        .sessions
        .push(SessionInfo::new(SessionId::new(2), "peer"));
    state.peers.windows.push(
        WindowInfo::new(WindowId::new(10), SessionId::new(2), "main")
            .with_index(0)
            .with_active_resource(Some(peer.clone())),
    );
    state
        .peers
        .resources
        .push(ResourceInfo::new(peer.clone(), WindowId::new(10), 80, 24));
    state.peers.foreign_agents.insert(
        peer,
        AgentRecord {
            name: "reviewer".into(),
            state: phux_client::agent_meta::AgentMetaState::Blocked,
            ..AgentRecord::default()
        },
    );
    state.peers.chrome_dirty = true;
    out.clear();
    state.drain_repaint(&mut out, sidebar, &mut RepaintAccumulator::default());
    let screen = painted_sidebar_text(&out);
    assert!(screen.contains("reviewer"), "{screen}");
    assert!(screen.contains("peer"), "{screen}");
}

/// Sweeping an unvisited peer GETs/SUBSCRIBEs its graph terminals.
#[tokio::test(flavor = "current_thread")]
async fn sweep_discovers_graph_terminals_before_layout_persist() {
    let (mut state, mut client, mut server, _) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    frames_sent(&mut client, &mut server).await;
    let peer = ResourceId::local(10);
    state
        .peers
        .sessions
        .push(SessionInfo::new(SessionId::new(2), "peer"));
    state
        .peers
        .windows
        .push(WindowInfo::new(WindowId::new(10), SessionId::new(2), "main").with_index(0));
    state
        .peers
        .resources
        .push(ResourceInfo::new(peer.clone(), WindowId::new(10), 80, 24));
    state.peers.sweep_pending = false;
    state.sweep_peer_layouts(&mut client).await.unwrap();
    let sent = frames_sent(&mut client, &mut server).await;
    assert!(
        sent.iter().any(|f| matches!(
            f,
            FrameKind::GetMetadata { scope, key, .. }
                if *scope == Scope::Resource(peer.clone())
                    && key == phux_client::agent_meta::RESOURCE_AGENT_KEY
        )),
        "graph terminal is fetched: {sent:?}"
    );
    assert!(
        sent.iter().any(|f| matches!(
            f,
            FrameKind::SubscribeMetadata { scope, .. }
                if *scope == Scope::Resource(peer.clone())
        )),
        "graph terminal is subscribed: {sent:?}"
    );
}

/// A resource-identity pick focuses the inventory pane even when
/// the destination still has the single-pane attach bootstrap.
#[tokio::test(flavor = "current_thread")]
async fn resource_pick_focuses_inventory_pane_without_a_tui_layout() {
    let (mut state, _, _, _) = bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let target = ResourceId::local(10);
    state.peers.windows.push(
        WindowInfo::new(WindowId::new(10), SessionId::new(1), "review")
            .with_index(1)
            .with_active_resource(Some(target.clone())),
    );
    state
        .peers
        .resources
        .push(ResourceInfo::new(target.clone(), WindowId::new(10), 80, 24));
    state.pick.resource = Some(target.clone());
    state.resolve_cross_session_pick();
    assert_eq!(state.mirror.focused_resource.as_ref(), Some(&target));
    assert!(
        state.mirror.workspace.groups.iter().any(|window| window
            .state
            .tree
            .as_ref()
            .is_some_and(|tree| crate::layout::leaves(tree).contains(&target))),
        "inventory window is adopted: {:?}",
        state.mirror.workspace.groups
    );
    state.pick.resource = Some(target.clone());
    state.mirror.workspace = ArrangementDoc::single(target.clone());
    state.mirror.workspace.groups[0].name = "review".into();
    state.resolve_cross_session_pick();
    assert_eq!(state.mirror.focused_resource.as_ref(), Some(&target));
    assert_eq!(state.mirror.workspace.groups.len(), 1);
}
