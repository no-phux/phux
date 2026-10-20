#[tokio::test(flavor = "current_thread")]
async fn bootstrap_subscribes_to_session_rename_key() {
    let (_state, mut client, mut server, _) = bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let sent = frames_sent(&mut client, &mut server).await;
    assert!(
        sent.iter().any(|frame| matches!(
            frame,
            FrameKind::SubscribeMetadata { scope: Scope::Global, key }
            if key == SESSION_NAME_KEY
        )),
        "attach must subscribe to SESSION_NAME_KEY so a peer rename refreshes the roster: {sent:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn session_rename_broadcast_updates_peer_roster_without_reattach() {
    use phux_protocol::wire::frame::encode_session_rename;

    let (mut state, mut client, _server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    state
        .peers
        .sessions
        .push(SessionInfo::new(SessionId::new(2), "peer"));
    state
        .apply_server_frame(
            &mut client,
            &mut out,
            None,
            FrameKind::MetadataChanged {
                scope: Scope::Global,
                key: SESSION_NAME_KEY.to_owned(),
                value: Some(encode_session_rename("peer", "notes")),
                actor: None,
            },
            true,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        state.mirror.session_name, "test",
        "a peer rename must not overwrite this client's status name"
    );
    let peer = state
        .peers
        .sessions
        .iter()
        .find(|session| session.id == SessionId::new(2))
        .expect("peer row remains");
    assert_eq!(peer.name, "notes");
}

/// A `phux.session.project/v1` broadcast fills the picker from the wire, not a test map.
#[tokio::test(flavor = "current_thread")]
async fn project_tag_broadcast_groups_the_session_picker() {
    use crate::attach::input_dispatch::session_picker_rows;
    use crate::render::overlay::select_list::SelectKind;
    use phux_protocol::wire::frame::{SESSION_PROJECT_KEY, encode_session_project};

    let (mut state, mut client, _server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    state
        .peers
        .sessions
        .push(SessionInfo::new(SessionId::new(2), "api"));
    state
        .peers
        .sessions
        .push(SessionInfo::new(SessionId::new(3), "web"));
    state
        .apply_server_frame(
            &mut client,
            &mut out,
            None,
            FrameKind::MetadataChanged {
                scope: Scope::Global,
                key: SESSION_PROJECT_KEY.to_owned(),
                value: Some(encode_session_project("api", "phux")),
                actor: None,
            },
            true,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        state
            .peers
            .project_tags
            .get(&SessionId::new(2))
            .map(String::as_str),
        Some("phux")
    );
    assert!(!state.peers.project_tags.contains_key(&SessionId::new(3)));
    let rows = session_picker_rows(
        &state
            .peers
            .inputs(&crate::attach::review::ReviewIndex::default()),
        &ArrangementDoc::default(),
    );
    assert!(
        rows.iter()
            .any(|row| row.label == "phux" && row.kind == SelectKind::Header),
        "the live picker must group by the wire project tag: {rows:?}"
    );
}

async fn answer_rename_barrier(
    state: &mut SessionLoop,
    client: &mut Connection,
    sessions: Vec<SessionInfo>,
) {
    use phux_protocol::wire::frame::{CommandResult, CommandValue};

    let barrier = state
        .rename_pending
        .as_ref()
        .expect("rename parked")
        .barrier;
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_sessions(sessions);
    let passed = state
        .intercept_peer_reply(
            client,
            FrameKind::CommandResult {
                request_id: barrier,
                result: CommandResult::OkWith(CommandValue::State(snapshot)),
            },
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    assert!(passed.is_none(), "the rename GET_STATE barrier is consumed");
}

fn parked_rename(state: &mut SessionLoop, new_name: &str) {
    state.rename_pending = Some(PendingSessionRename {
        barrier: 4242,
        session_id: Some(SessionId::new(1)),
        current: "test".to_owned(),
        new_name: new_name.to_owned(),
        organization: false,
    });
}

/// A `SESSION_RENAME` parked against an `ORGANIZATION` server.
fn parked_org_rename(state: &mut SessionLoop, new_name: &str) {
    parked_rename(state, new_name);
    if let Some(pending) = state.rename_pending.as_mut() {
        pending.organization = true;
    }
}

async fn answer_org_rename(
    state: &mut SessionLoop,
    client: &mut Connection,
    result: phux_protocol::wire::frame::CommandResult,
) {
    let passed = state
        .intercept_peer_reply(
            client,
            FrameKind::CommandResult {
                request_id: 4242,
                result,
            },
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    assert!(passed.is_none(), "the SESSION_RENAME reply is consumed");
    assert!(state.rename_pending.is_none());
}

/// ADR-0161: a committed `SESSION_RENAME` renames the status line and the
/// roster from its reply alone, with no `GET_STATE` barrier.
#[tokio::test(flavor = "current_thread")]
async fn committed_session_rename_applies_the_new_name() {
    use phux_protocol::ids::{OperationId, Revision};
    use phux_protocol::wire::frame::{CommandResult, CommandValue, Committed};
    let (mut state, mut client, _server, _) = bootstrapped_loop_with(ServerFeatureSet::new()).await;
    parked_org_rename(&mut state, "notes");
    let committed = Committed::new(OperationId::new([3; 16]).unwrap(), Revision::new(2));
    answer_org_rename(
        &mut state,
        &mut client,
        CommandResult::OkWith(CommandValue::Committed(Box::new(committed))),
    )
    .await;
    assert_eq!(state.mirror.session_name, "notes");
    let ours = state
        .peers
        .sessions
        .iter()
        .find(|session| session.id == SessionId::new(1))
        .expect("this session remains");
    assert_eq!(ours.name, "notes");
}

/// A refused `SESSION_RENAME` keeps the current name and says why.
#[tokio::test(flavor = "current_thread")]
async fn refused_session_rename_keeps_the_current_status_name() {
    use phux_protocol::wire::frame::{CommandResult, Refusal};
    let (mut state, mut client, _server, _) = bootstrapped_loop_with(ServerFeatureSet::new()).await;
    parked_org_rename(&mut state, "notes");
    answer_org_rename(
        &mut state,
        &mut client,
        CommandResult::Refused(Refusal::NameTaken),
    )
    .await;
    assert_eq!(state.mirror.session_name, "test");
    assert!(
        state
            .peers
            .sessions
            .iter()
            .all(|session| session.name != "notes")
    );
}

/// ADR-0161: an `ORGANIZATION` attach follows renames and lifetime changes
/// as events, so it never subscribes to the translated session keys.
#[tokio::test(flavor = "current_thread")]
async fn organization_attach_skips_the_translated_session_keys() {
    use phux_protocol::wire::frame::{CONFIG_RELOAD_KEY, SESSION_KEEP_EMPTY_KEY};
    let (mut state, mut client, mut server, _) = Box::pin(loop_before_drain_ext(
        ServerFeatureSet::new(),
        phux_protocol::ServerFeatureExtSet::with(&[phux_protocol::ServerFeatureExt::Organization]),
    ))
    .await;
    state
        .emit_deferred_bootstrap_outbound(&mut client)
        .await
        .unwrap();
    let sent = frames_sent(&mut client, &mut server).await;
    let subscribed = |wanted: &str| {
        sent.iter().any(|frame| {
            matches!(frame, FrameKind::SubscribeMetadata { scope: Scope::Global, key } if key == wanted)
        })
    };
    assert!(subscribed(CONFIG_RELOAD_KEY), "{sent:?}");
    assert!(!subscribed(SESSION_NAME_KEY), "{sent:?}");
    assert!(!subscribed(SESSION_KEEP_EMPTY_KEY), "{sent:?}");
}

/// ADR-0161: a `SESSION_RENAMED` event renames the peer row its key names.
#[tokio::test(flavor = "current_thread")]
async fn session_renamed_event_updates_the_peer_roster_by_key() {
    use phux_protocol::ids::{Revision, SessionKey};
    let (mut state, mut client, _server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let peer = SessionKey::new([2; 16]).unwrap();
    state
        .peers
        .sessions
        .push(SessionInfo::new(SessionId::new(2), "peer").with_key(Some(peer)));
    state
        .apply_server_frame(
            &mut client,
            &mut out,
            None,
            FrameKind::Event {
                terminal: None,
                event: phux_protocol::wire::frame::AgentEvent::SessionRenamed {
                    session: peer,
                    name: "notes".to_owned(),
                    revision: Revision::new(2),
                },
                stamp: None,
            },
            true,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    assert_eq!(state.mirror.session_name, "test");
    let names: Vec<&str> = state
        .peers
        .sessions
        .iter()
        .map(|session| session.name.as_str())
        .collect();
    assert!(names.contains(&"notes"), "{names:?}");
}

/// A `SESSION_PROJECT_SET` event sets, then clears, the peer row's
/// `project` attribute the sidebar groups by.
#[tokio::test(flavor = "current_thread")]
async fn session_project_set_event_relabels_the_peer_row_by_key() {
    use phux_protocol::ids::{Revision, SessionKey};
    let (mut state, mut client, _server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let peer = SessionKey::new([2; 16]).unwrap();
    state.peers.sessions.push(
        SessionInfo::new(SessionId::new(2), "peer")
            .with_key(Some(peer))
            .with_attrs(vec![("host".to_owned(), "mini".to_owned())]),
    );
    for (project, revision) in [(Some("phux"), 2), (None, 3)] {
        state
            .apply_server_frame(
                &mut client,
                &mut out,
                None,
                FrameKind::Event {
                    terminal: None,
                    event: phux_protocol::wire::frame::AgentEvent::SessionProjectSet {
                        session: peer,
                        project: project.map(str::to_owned),
                        revision: Revision::new(revision),
                    },
                    stamp: None,
                },
                true,
                &mut RepaintAccumulator::default(),
            )
            .await
            .unwrap();
        let row = state
            .peers
            .sessions
            .iter()
            .find(|session| session.key == Some(peer))
            .unwrap();
        assert_eq!(phux_client::project_identity::link_of_session(row), project);
        assert!(row.attrs.contains(&("host".to_owned(), "mini".to_owned())));
    }
}
