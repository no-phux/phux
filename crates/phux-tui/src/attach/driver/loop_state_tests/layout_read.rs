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
            "rename tab",
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

async fn assert_no_layout_write(client: &mut Connection, server: &mut Connection) {
    let sent = frames_sent(client, server).await;
    assert!(
        !sent
            .iter()
            .any(|frame| matches!(frame, FrameKind::SetMetadata { .. })),
        "layout written before initial read"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn queued_rename_cannot_write_before_initial_metadata_is_processed() {
    for metadata in [Some(b"\xa1\x67version\x02".to_vec()), None] {
        let (mut state, mut client, mut server, mut out) =
            bootstrapped_loop_with(ServerFeatureSet::new()).await;
        let request_id = state.layout_get_request_id.unwrap();
        let before = state.mirror.workspace.clone();
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
        assert_eq!(state.mirror.workspace, before);
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
            assert_eq!(state.mirror.workspace, before);
            assert!(!state.layout_read_complete);
            assert_no_layout_write(&mut client, &mut server).await;
        } else {
            result.unwrap();
            assert!(state.layout_read_complete);
            assert!(state.layout_get_request_id.is_none());
            // #1207: the absent key is first re-seeded with the bootstrap.
            let healed = frames_sent(&mut client, &mut server).await;
            assert!(
                healed
                    .iter()
                    .any(|frame| matches!(frame, FrameKind::SetMetadata { .. })),
                "an absent key is saved once read: {healed:?}"
            );
            rename_from_prompt(&mut state, &mut client, &mut out).await;
            let written = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if let FrameKind::SetMetadata { value, .. } = server.recv().await.unwrap() {
                        break ArrangementDoc::decode_cbor(&value).unwrap();
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(written.groups[0].name, "renamed");
        }
        drop(client);
        drop(server);
    }
}

/// #1475: a reopen pressed before the layout read is applied to the
/// arrangement that read adopts, then saved. An organization server does
/// not place hidden members on its own, so the extra window is the queued
/// edit and nothing else.
#[tokio::test(flavor = "current_thread")]
async fn a_deferred_reopen_is_saved_when_the_layout_read_lands() {
    use crate::attach::input_dispatch::DeferredArrangementEdit;

    let (mut state, mut client, mut server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    state.organization_supported = true;
    state.mirror.members.insert(ResourceId::local(2));
    state
        .deferred_arrangement_edits
        .push(DeferredArrangementEdit::ReopenViews);
    let _ = frames_sent(&mut client, &mut server).await;
    let request_id = state.layout_get_request_id.expect("layout get in flight");
    let stored = state.mirror.workspace.encode_cbor().unwrap();
    assert_eq!(state.mirror.workspace.groups.len(), 1);
    state
        .apply_server_frame(
            &mut client,
            &mut out,
            None,
            FrameKind::MetadataChanged {
                scope: Scope::Group(DEFAULT_GROUP_ID),
                key: layout_key(state.peers.focused_session.unwrap()),
                value: Some(stored.clone()),
                actor: None,
            },
            false,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    assert!(!state.layout_read_complete);
    assert_eq!(state.deferred_arrangement_edits.len(), 1);
    assert_eq!(state.mirror.workspace.groups.len(), 1);
    server
        .send(&FrameKind::MetadataValue {
            request_id,
            value: Some(stored),
        })
        .await
        .unwrap();
    let reply = client.recv().await.unwrap();
    state
        .apply_server_frame(
            &mut client,
            &mut out,
            None,
            reply,
            false,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    assert!(state.layout_read_complete);
    assert!(
        state.deferred_arrangement_edits.is_empty(),
        "the read consumes the queue"
    );
    assert_eq!(state.mirror.workspace.groups.len(), 2);
    let saved = frames_sent(&mut client, &mut server).await;
    let layout = saved.into_iter().find_map(|frame| match frame {
        FrameKind::SetMetadata { value, .. } => Some(ArrangementDoc::decode_cbor(&value).unwrap()),
        _ => None,
    });
    let layout = layout.expect("the queued reopen is saved once the read lands");
    assert_eq!(layout.groups.len(), 2);
    drop(client);
    drop(server);
}

/// One arranged and one hidden shell, or no arrangement key at all.
fn deferred_edit_fixture(
    present: bool,
) -> (
    phux_client::testkit::OrgFixture,
    SessionId,
    ResourceId,
    ResourceId,
) {
    use phux_client::testkit::OrgFixture;
    use phux_protocol::wire::frame::{
        ArrangementDoc as WireDoc, ArrangementGroup, ArrangementNode, EntryRef, EntryTarget,
        SessionLifetime,
    };

    let mut fixture = OrgFixture::new();
    let session = fixture.session("test", SessionLifetime::Persistent);
    let shell = fixture.shell(session);
    let hidden = fixture.shell(session);
    if present {
        fixture.arrangement(
            session,
            WireDoc {
                groups: vec![ArrangementGroup {
                    id: [7; 16],
                    name: Some("stored".to_owned()),
                    tree: ArrangementNode::Leaf(EntryRef {
                        entry: [1; 16],
                        target: EntryTarget::Member(shell),
                    }),
                }],
            },
        );
    }
    let session = fixture.live_session(&session).unwrap();
    let shell = fixture.live(&shell).unwrap();
    let hidden = fixture.live(&hidden).unwrap();
    (fixture, session, shell, hidden)
}

/// A failed organization read must keep the queued edit until recovery,
/// then persist it through the real arrangement link, never the legacy key.
#[tokio::test(flavor = "current_thread")]
async fn deferred_edits_survive_a_failed_arrangement_read_and_are_saved_on_recovery() {
    use crate::attach::input_dispatch::DeferredArrangementEdit;
    use phux_client::testkit::{ScriptSpec, serve_one};

    for (reopen, present) in [(true, true), (false, true), (true, false), (false, false)] {
        let (fixture, session, shell, hidden) = deferred_edit_fixture(present);
        let edit = if reopen {
            DeferredArrangementEdit::ReopenViews
        } else {
            DeferredArrangementEdit::CloseView {
                target: shell.clone(),
            }
        };
        let dir = tempfile::tempdir().unwrap();
        let (mut state, mut client, mut server, mut out) =
            bootstrapped_loop_with(ServerFeatureSet::new()).await;
        let _ = frames_sent(&mut client, &mut server).await;
        state.organization_supported = true;
        state.set_control_dial(crate::attach::Dial::uds(&dir.path().join("phux.sock")));
        state.peers.focused_session = Some(session);
        state.mirror.workspace = ArrangementDoc::single(shell.clone());
        state.mirror.focused_resource = Some(shell.clone());
        state.mirror.panes.clear();
        state
            .mirror
            .panes
            .insert(shell.clone(), PaneSlot::new_with_size(80, 24).unwrap());
        state.mirror.members = HashSet::from([shell, hidden]);
        state.subscribe_bootstrap(&mut client, true).await.unwrap();
        state.peer_reader = None;
        state.deferred_arrangement_edits.push(edit.clone());
        let before = state.mirror.workspace.clone();
        state
            .settle_arrangement_read(
                &mut client,
                &mut out,
                None,
                &mut RepaintAccumulator::default(),
            )
            .await
            .unwrap();
        assert_eq!(state.deferred_arrangement_edits, vec![edit]);
        assert_eq!(state.mirror.workspace, before, "the fallback is not edited");

        let spec = ScriptSpec::new()
            .server_features_ext(phux_protocol::ServerFeatureExtSet::with(&[
                phux_protocol::ServerFeatureExt::Organization,
            ]))
            .organization(fixture);
        let (_socket, org_server) = serve_one(dir.path(), spec);
        let mut bindings = phux_config::KeybindingsCfg::default();
        bindings.prefix_table.insert(
            "r".to_owned(),
            phux_config::Action::Bare(
                if reopen { "reopen-views" } else { "close-view" }.to_owned(),
            ),
        );
        state.settings.resolver = Some(phux_config::keybind::Resolver::new(&bindings).unwrap());
        state.settings.keybindings = Some(bindings);
        let mut events = Vec::new();
        state.parser.feed_into(b"\x01r", &mut events);
        state
            .dispatch_batch(&mut client, &mut out, None, events)
            .await
            .unwrap();
        assert!(state.deferred_arrangement_edits.is_empty());
        assert_eq!(
            state.mirror.workspace.groups.len(),
            if reopen { 2 } else { 0 },
            "replayed workspace, members: {:?}",
            state.mirror.members
        );
        let stored = state.arrangement.as_mut().unwrap().read().await.unwrap();
        if present {
            let stored = stored.unwrap();
            assert_eq!(stored.groups.len(), if reopen { 2 } else { 0 });
            if reopen {
                assert_eq!(stored.groups[0].name, "stored");
            }
        } else {
            // A session with no arrangement key cannot accept a replace,
            // but the authoritative absent read must not strand the queue.
            assert!(stored.is_none());
        }
        let sent = frames_sent(&mut client, &mut server).await;
        assert!(
            !sent
                .iter()
                .any(|frame| matches!(frame, FrameKind::SetMetadata { .. }))
        );
        state.arrangement = None;
        tokio::time::timeout(Duration::from_secs(5), org_server)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn deferred_focus_edits_clear_predictions_and_suspend_without_a_known_cursor() {
    use crate::attach::input_dispatch::DeferredArrangementEdit;

    for edit in [
        DeferredArrangementEdit::ReopenViews,
        DeferredArrangementEdit::CloseView {
            target: ResourceId::local(1),
        },
    ] {
        let (mut state, _client, _server, _out) =
            bootstrapped_loop_with(ServerFeatureSet::new()).await;
        state.mirror.members.insert(ResourceId::local(2));
        state.layout_read_complete = true;
        state.mirror.predict = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        state.mirror.predict.set_cursor(4, 9);
        state.mirror.predict.predict_key(&KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::A,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: Some("a".to_owned()),
            unshifted_codepoint: None,
        });
        assert_eq!(state.mirror.predict.pending_len(), 1);
        state.deferred_arrangement_edits.push(edit);
        assert!(state.flush_deferred_arrangement_edits());
        assert_eq!(state.mirror.predict.pending_len(), 0);
        assert!(state.mirror.predict.is_suspended());
    }
}

// ---- phux-c2td.20: orphaned satellite spawns ------------------------------
