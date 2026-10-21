/// `C-a H` moved the divider on screen but never resized the panes' PTYs:
/// the batch reflowed only on a zoom or sidebar change, so the shells kept
/// their old width and painted letterboxed (or cropped) in the new tiles.
/// A resize of the active window's own layout must send `RESIZE_TERMINAL`
/// for the leaves whose rect moved.
#[tokio::test(flavor = "current_thread")]
async fn resize_pane_resizes_the_panes_whose_tiles_moved() {
    use crate::layout::{Group, LayoutNode, LayoutState, SplitDir, split_at};
    use phux_config::keybind::parse_chord;

    let (mut state, mut client, mut server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let config = phux_config::parse_with_defaults("", std::path::Path::new("t.toml"))
        .expect("shipped defaults parse");
    state.settings = crate::settings::TuiSettings::tolerant_from(&config);
    let (left, right) = (ResourceId::local(1), ResourceId::local(2));
    let tree = split_at(
        &LayoutNode::Leaf(left.clone()),
        &left,
        &right,
        SplitDir::Horizontal,
        0.5,
    )
    .expect("split");
    state.mirror.workspace = ArrangementDoc {
        groups: vec![Group::new(
            "0".to_owned(),
            LayoutState {
                tree: Some(tree),
                focus: Some(right.clone()),
            },
        )],
        active: 0,
    };
    state.mirror.focused_resource = Some(right.clone());
    drop(frames_sent(&mut client, &mut server).await);

    let press = |spec: &str| {
        let chord = parse_chord(spec).expect("chord");
        InputEvent::Key(KeyEvent {
            action: KeyAction::Press,
            key: chord.key,
            mods: chord.modifiers,
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        })
    };
    // Layout actions wait for the initial shared layout read.
    state.layout_read_complete = true;
    // The loop's own sidebar, so only the resize can change the geometry.
    let sidebar = state.sidebar();
    state
        .dispatch_batch(
            &mut client,
            &mut out,
            sidebar,
            vec![press("C-a"), press("H")],
        )
        .await
        .expect("dispatch");

    let resized: Vec<ResourceId> = frames_sent(&mut client, &mut server)
        .await
        .into_iter()
        .filter_map(|frame| match frame {
            FrameKind::ResizeTerminal { terminal_id, .. } => Some(terminal_id),
            _ => None,
        })
        .collect();
    assert!(
        resized.contains(&left) && resized.contains(&right),
        "both tiles moved, so both PTYs must be resized; sent {resized:?}"
    );
    drop(client);
    drop(server);
}

/// #1207: a session whose layout key is missing, or names fewer panes than
/// the session owns, must not hide the rest. Every live session pane gets a
/// place, the attach focus stays, a peer session's pane stays out, and the
/// healed layout is saved.
#[tokio::test(flavor = "current_thread")]
async fn missing_or_partial_layout_places_every_session_pane_and_saves() {
    let focus = ResourceId::local(1);
    let hidden = ResourceId::local(4);
    let foreign = ResourceId::local(7);
    let stored_partial = ArrangementDoc::single(focus.clone()).encode_cbor().unwrap();
    for stored in [None, Some(stored_partial)] {
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let mut client = Connection::from_stream(a);
        let mut server = Connection::from_stream(b);
        let negotiated = NegotiatedBootstrap {
            profile: BootstrapProfile::SynthesizedVtRaw,
            limits: BootstrapLimits::default(),
            server_features: ServerFeatureSet::new(),
            server_features_ext: phux_protocol::ServerFeatureExtSet::new(),
        };
        let mut state = SessionLoop::new(
            negotiated,
            PredictiveConfig::disabled(),
            false,
            None,
            EntryPick::default(),
            None,
        )
        .unwrap();
        let mut out = Vec::new();
        let attached = FrameKind::Attached {
            attach_id: 1,
            initial_client_id: ClientId::new(1),
            snapshot: SessionSnapshot::new(SessionId::new(1), WindowId::new(1), focus.clone())
                .with_sessions(vec![
                    SessionInfo::new(SessionId::new(1), "default"),
                    SessionInfo::new(SessionId::new(2), "other"),
                ])
                .with_windows(vec![
                    WindowInfo::new(WindowId::new(1), SessionId::new(1), "window-0"),
                    WindowInfo::new(WindowId::new(2), SessionId::new(2), "window-0"),
                ])
                .with_resources(vec![
                    ResourceInfo::new(focus.clone(), WindowId::new(1), 80, 24),
                    ResourceInfo::new(hidden.clone(), WindowId::new(1), 80, 24),
                    ResourceInfo::new(foreign.clone(), WindowId::new(2), 80, 24),
                ]),
        };
        state
            .bootstrap(&mut client, &mut out, attached, None)
            .await
            .unwrap();
        state
            .emit_deferred_bootstrap_outbound(&mut client)
            .await
            .unwrap();
        let request_id = state.layout_get_request_id.unwrap();
        state
            .apply_server_frame(
                &mut client,
                &mut out,
                None,
                FrameKind::MetadataValue {
                    request_id,
                    value: stored.clone(),
                },
                false,
                &mut RepaintAccumulator::default(),
            )
            .await
            .unwrap();

        let leaves: Vec<ResourceId> = state
            .mirror
            .workspace
            .groups
            .iter()
            .filter_map(|group| group.state.tree.as_ref())
            .flat_map(crate::layout::leaves)
            .collect();
        assert_eq!(leaves, [focus.clone(), hidden.clone()], "stored {stored:?}");
        assert_eq!(state.mirror.workspace.active, 0);
        assert_eq!(state.mirror.focused_resource.as_ref(), Some(&focus));
        let saved = frames_sent(&mut client, &mut server)
            .await
            .into_iter()
            .find_map(|frame| match frame {
                FrameKind::SetMetadata { value, .. } => {
                    Some(ArrangementDoc::decode_cbor(&value).unwrap())
                }
                _ => None,
            })
            .expect("the healed layout is saved");
        assert_eq!(saved.groups.len(), 2, "stored {stored:?}");
        drop(client);
        drop(server);
    }
}

/// R15 against #1207's heal: on an `ORGANIZATION` server the layout is the
/// session's arrangement record, so a member the record leaves out was
/// closed on purpose. It stays unarranged and nothing is written back.
#[tokio::test(flavor = "current_thread")]
async fn an_organization_record_keeps_a_closed_view_unarranged() {
    let focus = ResourceId::local(1);
    let hidden = ResourceId::local(4);
    let stored = ArrangementDoc::single(focus.clone()).encode_cbor().unwrap();
    let (a, b) = tokio::net::UnixStream::pair().unwrap();
    let mut client = Connection::from_stream(a);
    let mut server = Connection::from_stream(b);
    let negotiated = NegotiatedBootstrap {
        profile: BootstrapProfile::SynthesizedVtRaw,
        limits: BootstrapLimits::default(),
        server_features: ServerFeatureSet::new(),
        server_features_ext: phux_protocol::ServerFeatureExtSet::with(&[
            phux_protocol::ServerFeatureExt::Organization,
        ]),
    };
    let mut state = SessionLoop::new(
        negotiated,
        PredictiveConfig::disabled(),
        false,
        None,
        EntryPick::default(),
        None,
    )
    .unwrap();
    let mut out = Vec::new();
    let attached = FrameKind::Attached {
        attach_id: 1,
        initial_client_id: ClientId::new(1),
        snapshot: SessionSnapshot::new(SessionId::new(1), WindowId::new(1), focus.clone())
            .with_sessions(vec![SessionInfo::new(SessionId::new(1), "default")])
            .with_windows(vec![WindowInfo::new(
                WindowId::new(1),
                SessionId::new(1),
                "window-0",
            )])
            .with_resources(vec![
                ResourceInfo::new(focus.clone(), WindowId::new(1), 80, 24),
                ResourceInfo::new(hidden.clone(), WindowId::new(1), 80, 24),
            ]),
    };
    state
        .bootstrap(&mut client, &mut out, attached, None)
        .await
        .unwrap();
    state
        .emit_deferred_bootstrap_outbound(&mut client)
        .await
        .unwrap();
    let request_id = state.layout_get_request_id.unwrap();
    let _ = frames_sent(&mut client, &mut server).await;
    state
        .apply_server_frame(
            &mut client,
            &mut out,
            None,
            FrameKind::MetadataValue {
                request_id,
                value: Some(stored),
            },
            false,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();

    let leaves: Vec<ResourceId> = state
        .mirror
        .workspace
        .groups
        .iter()
        .filter_map(|group| group.state.tree.as_ref())
        .flat_map(crate::layout::leaves)
        .collect();
    assert_eq!(leaves, std::slice::from_ref(&focus));
    assert_eq!(state.mirror.unarranged_members(), vec![hidden]);
    let saved = frames_sent(&mut client, &mut server)
        .await
        .into_iter()
        .any(|frame| matches!(frame, FrameKind::SetMetadata { .. }));
    drop(client);
    drop(server);
    assert!(!saved, "a deliberate closed view is not written back");
}

// ---------- repaint classification (the flicker fix) ----------
