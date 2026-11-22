/// phux-ytxc gap (c): on an `ORGANIZATION` server a control connection that
/// cannot open is a failed arrangement read, reported, that releases the
/// write fence; the attach never falls back to the layout key.
#[tokio::test(flavor = "current_thread")]
async fn an_unreachable_arrangement_link_never_falls_back_to_the_layout_key() {
    let dir = tempfile::tempdir().unwrap();
    let (mut state, mut client, mut server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let _ = frames_sent(&mut client, &mut server).await;
    state.organization_supported = true;
    state.set_control_dial(crate::attach::Dial::uds(&dir.path().join("absent.sock")));
    let session = state.peers.focused_session.expect("a focused session");

    state.subscribe_bootstrap(&mut client, true).await.unwrap();
    assert!(
        state.arrangement.is_some(),
        "the link is set without connecting"
    );
    assert!(state.arrangement_read_owed);
    assert!(state.layout_get_request_id.is_none());
    state
        .settle_arrangement_read(
            &mut client,
            &mut out,
            None,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    assert!(
        state.layout_read_complete,
        "the failed read releases the fence"
    );
    let sent = frames_sent(&mut client, &mut server).await;
    assert!(
        !sent.iter().any(|frame| matches!(
            frame,
            FrameKind::GetMetadata { key, .. }
                | FrameKind::SubscribeMetadata { key, .. }
                | FrameKind::SetMetadata { key, .. }
                if *key == phux_client::layout_ops::layout_key(session)
        )),
        "the layout key is never used: {sent:?}"
    );
}

/// A failed first arrangement read is "not loaded", never "absent": nothing
/// is healed or written, and the read is retried before any write.
#[tokio::test(flavor = "current_thread")]
async fn a_failed_first_arrangement_read_never_overwrites_the_stored_record() {
    use phux_client::testkit::{OrgFixture, ScriptSpec, serve_one};
    use phux_protocol::wire::frame::{
        ArrangementDoc as WireDoc, ArrangementGroup, ArrangementNode, Command, EntryRef,
        EntryTarget, SessionLifetime,
    };
    let mut fixture = OrgFixture::new();
    let ours = fixture.session("test", SessionLifetime::Persistent);
    let shell = fixture.shell(ours);
    fixture.arrangement(
        ours,
        WireDoc {
            groups: vec![ArrangementGroup {
                locked: false,
                id: [7; 16],
                name: Some("build".to_owned()),
                tree: ArrangementNode::Leaf(EntryRef::new([1; 16], EntryTarget::Member(shell))),
            }],
        },
    );
    let our_id = fixture.live_session(&ours).unwrap();
    let dir = tempfile::tempdir().unwrap();

    let (mut state, mut client, mut server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let _ = frames_sent(&mut client, &mut server).await;
    state.organization_supported = true;
    // The dial names a socket that is not up yet.
    state.set_control_dial(crate::attach::Dial::uds(&dir.path().join("phux.sock")));
    state.peers.focused_session = Some(our_id);
    state.subscribe_bootstrap(&mut client, true).await.unwrap();
    // The peer reader dials the same socket; this test serves one link.
    state.peer_reader = None;
    state
        .settle_arrangement_read(
            &mut client,
            &mut out,
            None,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    assert!(state.layout_read_complete, "the fence is released");
    assert!(
        !state.arrangement.as_ref().unwrap().loaded(),
        "a failed read is not a loaded absent record"
    );
    assert!(
        !state.arrangement_read_owed,
        "a failed read does not spin; the next operation owes it again"
    );

    // The socket comes up; a local edit asks to write before any read.
    let spec = ScriptSpec::new()
        .server_features_ext(phux_protocol::ServerFeatureExtSet::with(&[
            phux_protocol::ServerFeatureExt::Organization,
        ]))
        .organization(fixture);
    let (_socket, org_server) = serve_one(dir.path(), spec);
    state.broadcast_layout(&mut client).await.unwrap();
    assert!(
        state.arrangement_read_owed,
        "an unsaved edit owes the read again"
    );
    state
        .settle_arrangement_read(
            &mut client,
            &mut out,
            None,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    assert!(
        state.arrangement.as_ref().unwrap().loaded(),
        "the retried read loads the record"
    );

    // Hang up the link and inspect what the server received.
    state.arrangement = None;
    let seen = tokio::time::timeout(Duration::from_secs(5), org_server)
        .await
        .expect("the server sees the hang-up")
        .unwrap();
    assert!(
        seen.iter().any(|frame| matches!(
            frame,
            FrameKind::Command {
                command: Command::ArrangementGet { .. },
                ..
            }
        )),
        "the record was read: {seen:?}"
    );
    assert!(
        !seen.iter().any(|frame| matches!(
            frame,
            FrameKind::Command {
                command: Command::ArrangementReplace { .. },
                ..
            }
        )),
        "the stored arrangement is never replaced: {seen:?}"
    );
}

/// An explicit empty record means every view is hidden, including after
/// a fresh attach has provisionally seeded a focused pane.
#[tokio::test(flavor = "current_thread")]
async fn an_empty_arrangement_keeps_the_bootstrap_pane_unarranged() {
    use phux_client::testkit::{OrgFixture, ScriptSpec, serve_one};
    use phux_protocol::wire::frame::{ArrangementDoc as WireDoc, Command, SessionLifetime};

    let mut org = OrgFixture::new();
    let session = org.session("test", SessionLifetime::Persistent);
    let shell = org.shell(session);
    org.arrangement(session, WireDoc { groups: Vec::new() });
    let session_id = org.live_session(&session).unwrap();
    let pane = org.live(&shell).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let spec = ScriptSpec::new().organization(org);
    let (socket, org_server) = serve_one(dir.path(), spec);

    let (mut state, mut client, _server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    state.organization_supported = true;
    state.set_control_dial(crate::attach::Dial::uds(&socket));
    state.peers.focused_session = Some(session_id);
    state.mirror.workspace = ArrangementDoc::single(pane.clone());
    state.mirror.focused_resource = Some(pane.clone());
    state.mirror.members = HashSet::from([pane.clone()]);
    state.subscribe_bootstrap(&mut client, true).await.unwrap();
    state
        .settle_arrangement_read(
            &mut client,
            &mut out,
            None,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();

    assert!(state.layout_read_complete);
    assert!(state.mirror.workspace.groups.is_empty());
    assert!(state.mirror.focused_resource.is_none());
    assert_eq!(state.mirror.unarranged_members(), vec![pane]);
    state.arrangement = None;
    let commands = tokio::time::timeout(Duration::from_secs(5), org_server)
        .await
        .unwrap()
        .unwrap();
    assert!(commands.iter().any(|frame| matches!(
        frame,
        FrameKind::Command {
            command: Command::ArrangementGet { .. },
            ..
        }
    )));
    assert!(!commands.iter().any(|frame| matches!(
        frame,
        FrameKind::Command {
            command: Command::ArrangementReplace { .. },
            ..
        }
    )));
}
