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
        let frames = frames_sent(&mut client, &mut server).await;
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
        frames_sent(&mut client, &mut server).await;
        state.request_serving_host(&mut client).await.unwrap();
        assert!(frames_sent(&mut client, &mut server).await.is_empty());
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
        .mirror
        .panes
        .insert(peer_id.clone(), PaneSlot::new_with_size(100, 24).unwrap());
    state
        .peers
        .sessions
        .push(SessionInfo::new(SessionId::new(2), "peer"));
    state
        .peers
        .foreign_layouts
        .insert(SessionId::new(2), ArrangementDoc::single(peer_id.clone()));
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
        state: phux_client::agent_meta::AgentMetaState::Blocked,
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
    // One machine paints no host line; the hostname still lands.
    assert_eq!(state.peers.serving_host.as_deref(), Some("remote-mini"));
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
        actor: None,
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
    frames_sent(&mut client, &mut server).await;
    // The layout key is session-scoped; production already has this session
    // in the ATTACHED graph from the peer sweep that opened the watch.
    state
        .peers
        .sessions
        .push(SessionInfo::new(SessionId::new(2), "peer"));
    let id = ResourceId::local(10);
    let frame = FrameKind::MetadataChanged {
        scope: Scope::Group(phux_client::layout_ops::DEFAULT_LAYOUT_GROUP_ID),
        key: phux_client::layout_ops::layout_key(SessionId::new(2)),
        value: Some(ArrangementDoc::single(id.clone()).encode_cbor().unwrap()),
        actor: None,
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
    let sent = frames_sent(&mut client, &mut server).await;
    assert!(sent.iter().any(|f| matches!(f, FrameKind::GetMetadata { scope: Scope::Resource(r), key, .. } if r == &id && key == phux_client::agent_meta::RESOURCE_AGENT_KEY)));
    assert!(sent.iter().any(|f| matches!(f, FrameKind::SubscribeMetadata { scope: Scope::Resource(r), key } if r == &id && key == phux_client::agent_meta::RESOURCE_AGENT_KEY)));
}
