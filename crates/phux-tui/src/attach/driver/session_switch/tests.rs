use super::*;
use phux_client::testkit::{OrgFixture, ScriptSpec, serve_one};

/// One scripted server in a fresh tempdir, dialed once.
async fn connect_one(
    spec: ScriptSpec,
) -> (
    tempfile::TempDir,
    phux_client::attach::connection::Connection,
    tokio::task::JoinHandle<Vec<FrameKind>>,
) {
    let dir = tempfile::tempdir().expect("temp dir");
    let (socket, server) = serve_one(dir.path(), spec);
    let conn = phux_client::attach::connection::Connection::connect(&socket)
        .await
        .expect("connect");
    (dir, conn, server)
}
use phux_protocol::caps::{ServerCapabilities, ServerFeatureExt, ServerFeatureExtSet};
use phux_protocol::ids::{Revision, SessionKey};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, OrgFilter};

fn create_target() -> ReattachTarget {
    ReattachTarget::Create {
        name: "work".into(),
        directory: Some("/tmp".into()),
        host: None,
    }
}

fn org_spec(fixture: OrgFixture) -> ScriptSpec {
    ScriptSpec::new()
        .server_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
        .organization(fixture)
}

async fn switch(
    conn: &mut Connection,
    target: ReattachTarget,
    pick: &mut EntryPick,
) -> Result<FrameKind, AttachError> {
    reattach_on_same_connection(
        conn,
        target,
        pick,
        &mut super::super::orphans::OrphanKills::default(),
        &mut crate::attach::review::ReviewIndex::new(),
        None,
    )
    .await
}

fn assert_no_selection_or_cleanup(seen: &[FrameKind]) {
    assert!(
        !seen.iter().any(|frame| matches!(
            frame,
            FrameKind::Attach { .. }
                | FrameKind::Detach
                | FrameKind::Command {
                    command: Command::SessionKill { .. } | Command::SessionDelete { .. },
                    ..
                }
        )),
        "must retain the old view and never kill accepted work: {seen:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn name_collision_refuses_without_selecting_existing_or_detaching() {
    let mut fixture = OrgFixture::new();
    fixture.session("work", SessionLifetime::Persistent);
    let (_dir, mut conn, server) = connect_one(org_spec(fixture)).await;
    let mut pick = EntryPick {
        window: Some(7),
        pane: Some(2),
        resource: None,
    };
    let result = switch(&mut conn, create_target(), &mut pick).await;
    assert!(matches!(result, Err(AttachError::Refused(_))), "{result:?}");
    assert_eq!(pick.window, Some(7));
    drop(conn);
    let seen = server.await.unwrap();
    assert_no_selection_or_cleanup(&seen);
    assert_eq!(
        seen.iter()
            .filter(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::SessionCreate { .. },
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(!seen.iter().any(|frame| matches!(
        frame,
        FrameKind::Command {
            command: Command::GetOrganization { .. } | Command::SessionEnsure { .. },
            ..
        }
    )));
}

#[tokio::test(flavor = "current_thread")]
async fn exact_committed_key_selects_renamed_session_not_reused_name() {
    let operation = org::mint_operation();
    let mut fixture = OrgFixture::new();
    fixture.session("work", SessionLifetime::Persistent);
    let committed_key = fixture.session("renamed-after-create", SessionLifetime::Persistent);
    let committed_id = fixture.live_session(&committed_key).unwrap();
    let receipt = Committed::new(operation, Revision::new(9)).with_session(Some(committed_key));
    let spec = org_spec(fixture).mutation_reply(CommandResult::OkWith(CommandValue::Committed(
        Box::new(receipt),
    )));
    let (_dir, mut conn, server) = connect_one(spec).await;
    let (receipt, target) =
        create_and_select(&mut conn, operation, "work".into(), Some("/tmp".into()))
            .await
            .unwrap();
    assert_eq!(receipt.session, Some(committed_key));
    assert_eq!(receipt.operation, operation);
    assert_eq!(target, AttachTarget::ById(committed_id));
    drop(conn);
    let seen = server.await.unwrap();
    assert!(seen.iter().any(|frame| matches!(frame,
        FrameKind::Command { command: Command::GetOrganization { filter: OrgFilter::Session(key), .. }, .. } if key == &committed_key
    )), "selection must query the durable key: {seen:?}");
    assert!(seen.iter().any(|frame| matches!(frame,
        FrameKind::Command { command: Command::SessionCreate { operation: id, lifetime: SessionLifetime::Persistent, launch: Some(launch), .. }, .. }
        if id == &operation && launch.command.is_empty() && launch.cwd.as_deref() == Some("/tmp")
    )));
}

#[tokio::test(flavor = "current_thread")]
async fn lost_receipt_retains_original_operation_without_duplicate_create() {
    use phux_wire_fixtures::HelloOk;
    let (a, b) = tokio::net::UnixStream::pair().unwrap();
    let mut conn = Connection::from_stream(a);
    let peer = tokio::spawn(async move {
        let mut server = Connection::from_stream(b);
        let FrameKind::Hello { client_caps, .. } = server.recv().await.unwrap() else {
            panic!("HELLO")
        };
        server
            .send(
                &HelloOk::new(Vec::new(), ServerCapabilities::new())
                    .features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
                    .negotiated_from(&client_caps)
                    .build(),
            )
            .await
            .unwrap();
        let FrameKind::Command {
            command: Command::SessionCreate { operation, .. },
            ..
        } = server.recv().await.unwrap()
        else {
            panic!("exact create must be the first effect")
        };
        // Lose the response after the host may have committed. Closing the
        // connection must not cause a fresh operation or legacy attach retry.
        operation
    });
    conn.negotiate(
        "tui-lost-receipt".into(),
        super::super::session_io::attach_client_caps(
            None,
            &crate::attach::Dial::uds(std::path::Path::new("/tmp/unused")),
        ),
    )
    .await
    .unwrap();
    let result = switch(&mut conn, create_target(), &mut EntryPick::default()).await;
    let original = peer.await.unwrap();
    let error = result.unwrap_err();
    assert!(
        !error.is_transport_loss(),
        "unknown creation must not enter reconnect-create"
    );
    assert!(
        matches!(error, AttachError::OperationOutcomeUnknown { operation: Some(id) } if id == original)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn accepted_without_creation_receipt_remains_unknown_and_does_not_attach() {
    let operation = org::mint_operation();
    let spec = org_spec(OrgFixture::new()).mutation_reply(CommandResult::Accepted { operation });
    let (_dir, mut conn, server) = connect_one(spec).await;
    let result = create_and_select(&mut conn, operation, "work".into(), None).await;
    assert!(
        matches!(result, Err(AttachError::OperationOutcomeUnknown { operation: Some(id) }) if id == operation)
    );
    drop(conn);
    let seen = server.await.unwrap();
    assert_no_selection_or_cleanup(&seen);
    assert_eq!(
        seen.iter()
            .filter(|frame| matches!(frame, FrameKind::Command { .. }))
            .count(),
        1
    );
}

#[tokio::test(flavor = "current_thread")]
async fn unavailable_committed_key_is_reported_without_name_fallback_or_kill() {
    let operation = org::mint_operation();
    let key = SessionKey::new([91; 16]).unwrap();
    let receipt = Committed::new(operation, Revision::new(3)).with_session(Some(key));
    let mut fixture = OrgFixture::new();
    fixture.session("work", SessionLifetime::Persistent);
    let spec = org_spec(fixture).mutation_reply(CommandResult::OkWith(CommandValue::Committed(
        Box::new(receipt),
    )));
    let (_dir, mut conn, server) = connect_one(spec).await;
    let result = create_and_select(&mut conn, operation, "work".into(), None).await;
    assert!(
        matches!(result, Err(AttachError::CommittedSelectionUnavailable { operation: id, session, .. }) if id == operation && session == key)
    );
    drop(conn);
    let seen = server.await.unwrap();
    assert_no_selection_or_cleanup(&seen);
    assert_eq!(
        seen.iter()
            .filter(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::SessionCreate { .. },
                    ..
                }
            ))
            .count(),
        1
    );
}

#[tokio::test(flavor = "current_thread")]
async fn unsupported_creation_accepts_no_hidden_work_and_retains_view() {
    let spec = ScriptSpec::new().server_features_ext(ServerFeatureExtSet::new());
    let (_dir, mut conn, server) = connect_one(spec).await;
    let result = switch(&mut conn, create_target(), &mut EntryPick::default()).await;
    assert!(matches!(result, Err(AttachError::Refused(_))));
    drop(conn);
    let seen = server.await.unwrap();
    assert!(
        matches!(seen.as_slice(), [FrameKind::Hello { .. }]),
        "{seen:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn satellite_creation_is_not_silently_redirected_to_attached_host() {
    let (_dir, mut conn, server) = connect_one(org_spec(OrgFixture::new())).await;
    let target = ReattachTarget::Create {
        name: "work".into(),
        directory: None,
        host: Some("satellite".into()),
    };
    assert!(matches!(
        switch(&mut conn, target, &mut EntryPick::default()).await,
        Err(AttachError::Refused(_))
    ));
    drop(conn);
    let seen = server.await.unwrap();
    assert!(
        matches!(seen.as_slice(), [FrameKind::Hello { .. }]),
        "{seen:?}"
    );
}

/// Answer the peer's `SESSION_CREATE` with a receipt committing `key`, and
/// return the creation's operation.
async fn commit_creation(server: &mut Connection, key: SessionKey) -> OperationId {
    let FrameKind::Command {
        request_id,
        command: Command::SessionCreate { operation, .. },
    } = server.recv().await.unwrap()
    else {
        panic!("creation must precede detach");
    };
    let receipt = Committed::new(operation, Revision::new(1)).with_session(Some(key));
    server
        .send(&FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::Committed(Box::new(receipt))),
        })
        .await
        .unwrap();
    operation
}

/// Answer the peer's exact-key lookup of `key` with the renamed session,
/// live as `live`.
async fn resolve_exact_key(
    server: &mut Connection,
    key: SessionKey,
    live: phux_protocol::ids::SessionId,
) {
    use phux_protocol::ids::AuthorityId;
    use phux_protocol::wire::frame::{OrganizationPage, SessionRecord};
    let FrameKind::Command {
        request_id,
        command:
            Command::GetOrganization {
                filter: OrgFilter::Session(selected),
                ..
            },
    } = server.recv().await.unwrap()
    else {
        panic!("exact-key resolution must precede detach");
    };
    assert_eq!(selected, key);
    let page = OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(1), 0)
        .with_sessions(vec![
            SessionRecord::new(key, "renamed", Revision::new(1)).with_live(Some(live)),
        ])
        .with_totals(1, 0, 0);
    server
        .send(&FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
        })
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn attach_refusal_after_commit_preserves_receipt_and_never_kills_session() {
    use phux_protocol::ids::SessionId;
    use phux_protocol::wire::frame::ErrorCode;
    use phux_wire_fixtures::HelloOk;
    let key = SessionKey::new([41; 16]).unwrap();
    let live = SessionId::new(51);
    let (a, b) = tokio::net::UnixStream::pair().unwrap();
    let mut conn = Connection::from_stream(a);
    let peer = tokio::spawn(async move {
        let mut server = Connection::from_stream(b);
        let FrameKind::Hello { client_caps, .. } = server.recv().await.unwrap() else {
            panic!("HELLO");
        };
        server
            .send(
                &HelloOk::new(Vec::new(), ServerCapabilities::new())
                    .features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
                    .negotiated_from(&client_caps)
                    .build(),
            )
            .await
            .unwrap();
        let operation = commit_creation(&mut server, key).await;
        resolve_exact_key(&mut server, key, live).await;
        assert!(matches!(server.recv().await.unwrap(), FrameKind::Detach));
        server
            .send(&FrameKind::Detached {
                reason: None,
                message: String::new(),
            })
            .await
            .unwrap();
        let FrameKind::Attach { target, .. } = server.recv().await.unwrap() else {
            panic!("ATTACH");
        };
        assert_eq!(target, AttachTarget::ById(live));
        server
            .send(&FrameKind::Error {
                request_id: None,
                code: ErrorCode::InvalidCommand,
                message: "selection unavailable".into(),
            })
            .await
            .unwrap();
        assert!(
            server.recv().await.is_err(),
            "no duplicate create or cleanup kill"
        );
        operation
    });
    let caps = super::super::session_io::attach_client_caps(
        None,
        &crate::attach::Dial::uds(std::path::Path::new("/tmp/unused")),
    );
    conn.negotiate("tui-attach-refused".into(), caps)
        .await
        .unwrap();
    let result = switch(&mut conn, create_target(), &mut EntryPick::default()).await;
    drop(conn);
    let original = peer.await.unwrap();
    let error = result.unwrap_err();
    assert!(
        !error.is_transport_loss(),
        "a committed session must never re-enter creation"
    );
    assert!(matches!(error, AttachError::CommittedSelectionUnavailable {
        operation, session, ..
    } if operation == original && session == key));
}
