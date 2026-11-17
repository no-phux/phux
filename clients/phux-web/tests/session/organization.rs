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

fn session_key(byte: u8) -> SessionKey {
    SessionKey::new([byte; 16]).expect("non-zero key")
}

fn organization_page(request_id: u32, page: OrganizationPage) -> FrameKind {
    let value = CommandValue::OrganizationPage(Box::new(page));
    reply(request_id, CommandResult::OkWith(value))
}

fn empty_page() -> OrganizationPage {
    OrganizationPage::new(
        AuthorityId::new([9; 16]).expect("non-zero authority"),
        Revision::new(1),
        0,
    )
}

/// A connection to a legacy server and the `GET_STATE` it sent.
async fn legacy() -> (Connection, u32) {
    let (session, hello) = greeted(hello_with(ServerCapabilities::new())).await;
    let request_id = state_request(&hello.send[1]);
    (session, request_id)
}

/// The request id of the command `frame` encodes.
fn command_id(frame: &[u8]) -> u32 {
    let FrameKind::Command { request_id, .. } = decode_one(frame) else {
        panic!("expected a command");
    };
    request_id
}

#[wasm_bindgen_test]
async fn a_legacy_server_attaches_its_most_recent_session_and_creates_nothing() {
    let (mut session, request_id) = legacy().await;
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
    let (mut session, request_id) = legacy().await;
    let reply = session.on_frame(state_reply(request_id, Vec::new()));
    assert!(matches!(
        attach_target(&reply.send[0]),
        AttachTarget::CreateIfMissing { name, .. } if name == "default"
    ));
}

#[wasm_bindgen_test]
async fn an_unreadable_state_attaches_the_last_session_without_creating() {
    let (mut session, request_id) = legacy().await;
    let reply = session.on_frame(command_err(
        request_id,
        ErrorCode::InvalidCommand,
        "no inventory",
    ));
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
    let (mut session, hello) = greeted(hello_with(organization_caps())).await;
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
    let (mut session, hello) = greeted(hello_with(organization_caps())).await;
    let request_id = command_id(&hello.send[1]);
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
    let (mut session, hello) = greeted(hello_with(organization_caps())).await;
    let request_id = command_id(&hello.send[1]);
    let reply = session.on_frame(reply(request_id, CommandResult::Refused(Refusal::NotFound)));
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
    let output = session.on_frame(Gen::on(second, 2).output(1, &b"still running"[..]));
    assert!(output.fatal.is_none());
    let refused = session.close_view().unwrap_err();
    assert!(refused.contains("last view"), "{refused}");
    assert!(session.key_frame(key()).is_some());
}

/// A two-pane connection to an `ORGANIZATION` server whose attached session
/// has `key`.
async fn organization_session(key: SessionKey) -> Connection {
    ready_session(
        hello_with(organization_caps()),
        vec![SessionInfo::new(SessionId::new(1), "intake").with_key(Some(key))],
        vec![ResourceId::local(101), ResourceId::local(102)],
    )
    .await
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

/// An [`organization_session`] focused on its second pane after asking to
/// kill it: the pane, and the session-page request the kill sent.
async fn killing_second(key: SessionKey) -> (Connection, ResourceId, u32) {
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
    (session, target, request_id)
}

#[wasm_bindgen_test]
async fn killing_on_an_organization_server_terminates_the_current_activation() {
    let key = session_key(4);
    let (mut session, target, request_id) = killing_second(key).await;
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
    let operation = phux_protocol::ids::OperationId::new([5; 16]).unwrap();
    session.on_frame(reply(request_id, CommandResult::Accepted { operation }));
    assert!(session.pane_pending(), "the pane stays until it closes");
    assert_eq!(session.pane_rects().len(), 2);
    session.on_frame(closed(target, CloseReason::Unknown));
    assert!(!session.pane_pending());
    assert_eq!(session.pane_rects().len(), 1);
}

#[wasm_bindgen_test]
async fn an_unlisted_kill_target_falls_back_and_a_refusal_keeps_the_pane() {
    let key = session_key(4);
    let (mut session, target, request_id) = killing_second(key).await;
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
    session.on_frame(command_err(
        request_id,
        ErrorCode::InvalidCommand,
        "not permitted",
    ));
    assert!(!session.pane_pending());
    assert_eq!(session.pane_rects().len(), 2);
    assert!(session.pane_error().unwrap().contains("not permitted"));
}

#[wasm_bindgen_test]
async fn a_refused_terminate_keeps_the_pane_and_says_why() {
    let key = session_key(4);
    let (mut session, target, request_id) = killing_second(key).await;
    let page = empty_page().with_resources(vec![running(
        ResourceKey::new([2; 16]).unwrap(),
        key,
        target,
        3,
    )]);
    let terminate = session.on_frame(organization_page(request_id, page));
    let request_id = command_id(&terminate.send[0]);
    let capability = "terminate".to_owned();
    session.on_frame(reply(
        request_id,
        CommandResult::Refused(Refusal::Unsupported { capability }),
    ));
    assert!(!session.pane_pending());
    assert_eq!(session.pane_rects().len(), 2);
    assert!(session.pane_error().unwrap().contains("unsupported"));
}
