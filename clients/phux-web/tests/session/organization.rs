//! Choosing a session to attach, and organization-server kills and views.

use super::*;

fn attach_target(frame: &[u8]) -> AttachTarget {
    match decode_one(frame) {
        FrameKind::Attach { target, .. } => target,
        other => panic!("expected ATTACH, got {other:?}"),
    }
}

fn organization_caps() -> ServerCapabilities {
    ServerCapabilities::new()
        .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
}

fn hello_with(caps: ServerCapabilities) -> FrameKind {
    let mut frame = hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    );
    if let FrameKind::HelloOk { server_caps, .. } = &mut frame {
        *server_caps = caps;
    }
    frame
}

fn session_key(byte: u8) -> SessionKey {
    SessionKey::new([byte; 16]).expect("non-zero key")
}

fn organization_page(request_id: u32, page: OrganizationPage) -> FrameKind {
    FrameKind::CommandResult {
        request_id,
        result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
    }
}

fn empty_page() -> OrganizationPage {
    OrganizationPage::new(
        AuthorityId::new([9; 16]).expect("non-zero authority"),
        Revision::new(1),
        0,
    )
}

#[wasm_bindgen_test]
async fn a_legacy_server_attaches_its_most_recent_session_and_creates_nothing() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 80, 24);
    let hello = session.on_frame(hello_with(ServerCapabilities::new()));
    let request_id = state_request(&hello.send[1]);
    let reply = session.on_frame(state_reply(
        request_id,
        vec![
            SessionInfo::new(SessionId::new(4), "old").with_created_at_unix_secs(10),
            SessionInfo::new(SessionId::new(2), "new").with_created_at_unix_secs(20),
        ],
    ));
    assert_eq!(
        attach_target(&reply.send[0]),
        AttachTarget::ById(SessionId::new(2))
    );
}

#[wasm_bindgen_test]
async fn only_an_empty_legacy_server_gets_a_default_session() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 80, 24);
    let hello = session.on_frame(hello_with(ServerCapabilities::new()));
    let request_id = state_request(&hello.send[1]);
    let reply = session.on_frame(state_reply(request_id, Vec::new()));
    assert!(matches!(
        attach_target(&reply.send[0]),
        AttachTarget::CreateIfMissing { name, .. } if name == "default"
    ));
}

#[wasm_bindgen_test]
async fn an_unreadable_state_attaches_the_last_session_without_creating() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 80, 24);
    let hello = session.on_frame(hello_with(ServerCapabilities::new()));
    let request_id = state_request(&hello.send[1]);
    let reply = session.on_frame(FrameKind::CommandResult {
        request_id,
        result: CommandResult::Error {
            code: phux_protocol::wire::frame::ErrorCode::InvalidCommand,
            message: "no inventory".to_owned(),
        },
    });
    assert_eq!(attach_target(&reply.send[0]), AttachTarget::Last);
    assert!(session.pane_error().is_none());
}

#[wasm_bindgen_test]
fn intake_attach_preference_is_default_and_keeps_the_legacy_alias() {
    assert_eq!(AttachPreference::default(), AttachPreference::Intake);
    assert_eq!(AttachPreference::Inbox, AttachPreference::Intake);
}

#[wasm_bindgen_test]
async fn an_organization_server_opens_intake() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 80, 24);
    let hello = session.on_frame(hello_with(organization_caps()));
    assert!(session.organization_supported());
    let request_id = match decode_one(&hello.send[1]) {
        FrameKind::Command {
            request_id,
            command:
                Command::GetOrganization {
                    filter: OrgFilter::Intake,
                    ..
                },
        } => request_id,
        other => panic!("expected GET_ORGANIZATION, got {other:?}"),
    };
    let inbox = session_key(3);
    let page = empty_page().with_intake(Some(inbox)).with_sessions(vec![
        SessionRecord::new(session_key(5), "imported", Revision::new(1))
            .with_awaiting_organization(true)
            .with_live(Some(SessionId::new(8))),
        SessionRecord::new(inbox, "intake", Revision::new(1))
            .with_members(1)
            .with_live(Some(SessionId::new(6))),
    ]);
    let reply = session.on_frame(organization_page(request_id, page));
    assert_eq!(
        attach_target(&reply.send[0]),
        AttachTarget::ById(SessionId::new(6))
    );
}

#[wasm_bindgen_test]
async fn empty_intake_falls_back_to_the_most_recent_session() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 80, 24);
    let hello = session.on_frame(hello_with(organization_caps()));
    let FrameKind::Command { request_id, .. } = decode_one(&hello.send[1]) else {
        panic!("expected a command");
    };
    let inbox = session_key(3);
    let page = empty_page().with_intake(Some(inbox)).with_sessions(vec![
        SessionRecord::new(inbox, "intake", Revision::new(1)).with_live(Some(SessionId::new(6))),
    ]);
    let reply = session.on_frame(organization_page(request_id, page));
    let request_id = state_request(&reply.send[0]);
    let reply = session.on_frame(state_reply(
        request_id,
        vec![SessionInfo::new(SessionId::new(7), "default")],
    ));
    assert_eq!(
        attach_target(&reply.send[0]),
        AttachTarget::ById(SessionId::new(7))
    );
}

#[wasm_bindgen_test]
async fn a_refused_organization_read_falls_back_to_the_session_list() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 80, 24);
    let hello = session.on_frame(hello_with(organization_caps()));
    let FrameKind::Command { request_id, .. } = decode_one(&hello.send[1]) else {
        panic!("expected a command");
    };
    let reply = session.on_frame(FrameKind::CommandResult {
        request_id,
        result: CommandResult::Refused(Refusal::NotFound),
    });
    let request_id = state_request(&reply.send[0]);
    let reply = session.on_frame(state_reply(
        request_id,
        vec![SessionInfo::new(SessionId::new(7), "work")],
    ));
    assert_eq!(
        attach_target(&reply.send[0]),
        AttachTarget::ById(SessionId::new(7))
    );
}

#[wasm_bindgen_test]
async fn a_page_asking_for_the_most_recent_session_skips_intake() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 80, 24);
    session.set_attach_preference(AttachPreference::MostRecent);
    let hello = session.on_frame(hello_with(organization_caps()));
    state_request(&hello.send[1]);
}

#[wasm_bindgen_test]
async fn closing_a_view_ends_nothing_and_gives_its_space_to_the_sibling() {
    let mut session = path_session(false, true).await;
    let first = ResourceId::local(101);
    let second = ResourceId::local(102);
    session.resize_panes(40, 3);
    session.focus_pane(&second).expect("focus second");
    let frames = session.close_view().expect("close view");
    assert!(!frames.is_empty());
    for frame in &frames {
        assert!(
            matches!(decode_one(frame), FrameKind::ResizeTerminal { terminal_id, .. } if terminal_id == first),
            "closing a view only resizes the remaining pane"
        );
    }
    assert!(!session.pane_pending());
    assert_eq!(session.pane_rects().len(), 1);
    assert_eq!(session.focused_pane(), Some(first.clone()));
    // The closed view's terminal is still attached and still streams.
    assert!(session.pane_terminal(&second).is_some());
    let output = session.on_frame(FrameKind::ResourceOutput {
        terminal_id: second,
        stream_id: stream(2),
        bootstrap_id: bootstrap(2),
        seq: 1,
        bytes: Bytes::from_static(b"still running"),
    });
    assert!(output.fatal.is_none());
    let refused = session.close_view().unwrap_err();
    assert!(refused.contains("last view"), "{refused}");
    assert!(session.key_frame(key()).is_some());
}

/// A two-pane connection to an `ORGANIZATION` server whose attached session
/// has `key`.
async fn organization_session(key: SessionKey) -> Connection {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 20, 3);
    session.on_frame(hello_with(organization_caps()));
    let panes = [ResourceId::local(101), ResourceId::local(102)];
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), panes[0].clone())
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "intake").with_key(Some(key)),
        ])
        .with_resources(
            panes
                .iter()
                .cloned()
                .map(|id| ResourceInfo::new(id, WindowId::new(1), 20, 3))
                .collect(),
        );
    session.on_frame(FrameKind::Attached {
        attach_id: 1,
        snapshot,
        initial_client_id: ClientId::new(1),
    });
    for (i, id) in panes.iter().enumerate() {
        let stream_id = stream(i as u64 + 1);
        let bootstrap_id = bootstrap(i as u64 + 1);
        session.on_frame(begin(
            id.clone(),
            stream_id,
            bootstrap_id,
            phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
            20,
            3,
            0,
        ));
        session.on_frame(FrameKind::BootstrapReady {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        });
    }
    session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    session
}

fn running(
    key: ResourceKey,
    session: SessionKey,
    live: ResourceId,
    activation: u64,
) -> ResourceRecord {
    ResourceRecord::new(key, session, Revision::new(1))
        .with_live(Some(live))
        .with_activation(Some(ActivationInfo::new(
            ActivationId::new(activation),
            ServerInstance::new([7; 16]),
            ExecState::Running,
        )))
}

#[wasm_bindgen_test]
async fn killing_on_an_organization_server_terminates_the_current_activation() {
    let key = session_key(4);
    let mut session = organization_session(key).await;
    let target = ResourceId::local(102);
    session.focus_pane(&target).expect("focus second");
    let request_id = match decode_one(&session.kill_pane_frame().expect("kill")) {
        FrameKind::Command {
            request_id,
            command:
                Command::GetOrganization {
                    filter: OrgFilter::Session(filter),
                    ..
                },
        } => {
            assert_eq!(filter, key);
            request_id
        }
        other => panic!("expected the session page, got {other:?}"),
    };
    assert!(session.pane_pending());
    let page = empty_page().with_resources(vec![
        running(
            ResourceKey::new([1; 16]).unwrap(),
            key,
            ResourceId::local(101),
            1,
        ),
        running(ResourceKey::new([2; 16]).unwrap(), key, target.clone(), 3),
    ]);
    let terminate = session.on_frame(organization_page(request_id, page));
    let request_id = match decode_one(&terminate.send[0]) {
        FrameKind::Command {
            request_id,
            command: Command::ResourceTerminate { target: live, .. },
        } => {
            assert_eq!(live.resource, target);
            assert_eq!(live.instance, ServerInstance::new([7; 16]));
            assert_eq!(live.activation, ActivationId::new(3));
            request_id
        }
        other => panic!("expected RESOURCE_TERMINATE, got {other:?}"),
    };
    session.on_frame(FrameKind::CommandResult {
        request_id,
        result: CommandResult::Accepted {
            operation: phux_protocol::ids::OperationId::new([5; 16]).unwrap(),
        },
    });
    assert!(session.pane_pending(), "the pane stays until it closes");
    assert_eq!(session.pane_rects().len(), 2);
    session.on_frame(FrameKind::ResourceClosed {
        terminal_id: target,
        exit_status: None,
        reason: phux_protocol::wire::frame::CloseReason::Unknown,
        signal: None,
    });
    assert!(!session.pane_pending());
    assert_eq!(session.pane_rects().len(), 1);
}

#[wasm_bindgen_test]
async fn an_unlisted_kill_target_falls_back_and_a_refusal_keeps_the_pane() {
    let key = session_key(4);
    let mut session = organization_session(key).await;
    let target = ResourceId::local(102);
    session.focus_pane(&target).expect("focus second");
    let FrameKind::Command { request_id, .. } = decode_one(&session.kill_pane_frame().unwrap())
    else {
        panic!("expected a command");
    };
    // A page set that does not list the target falls back to KILL_RESOURCE.
    let fallback = session.on_frame(organization_page(request_id, empty_page()));
    let request_id = match decode_one(&fallback.send[0]) {
        FrameKind::Command {
            request_id,
            command: Command::KillResource { terminal_id, .. },
        } => {
            assert_eq!(terminal_id, target);
            request_id
        }
        other => panic!("expected the legacy kill, got {other:?}"),
    };
    session.on_frame(FrameKind::CommandResult {
        request_id,
        result: CommandResult::Error {
            code: phux_protocol::wire::frame::ErrorCode::InvalidCommand,
            message: "not permitted".to_owned(),
        },
    });
    assert!(!session.pane_pending());
    assert_eq!(session.pane_rects().len(), 2);
    assert!(session.pane_error().unwrap().contains("not permitted"));
}

#[wasm_bindgen_test]
async fn a_refused_terminate_keeps_the_pane_and_says_why() {
    let key = session_key(4);
    let mut session = organization_session(key).await;
    let target = ResourceId::local(102);
    session.focus_pane(&target).expect("focus second");
    let FrameKind::Command { request_id, .. } = decode_one(&session.kill_pane_frame().unwrap())
    else {
        panic!("expected a command");
    };
    let page = empty_page().with_resources(vec![running(
        ResourceKey::new([2; 16]).unwrap(),
        key,
        target,
        3,
    )]);
    let terminate = session.on_frame(organization_page(request_id, page));
    let FrameKind::Command { request_id, .. } = decode_one(&terminate.send[0]) else {
        panic!("expected a command");
    };
    session.on_frame(FrameKind::CommandResult {
        request_id,
        result: CommandResult::Refused(Refusal::Unsupported {
            capability: "terminate".to_owned(),
        }),
    });
    assert!(!session.pane_pending());
    assert_eq!(session.pane_rects().len(), 2);
    assert!(session.pane_error().unwrap().contains("unsupported"));
}
