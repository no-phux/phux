#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]
use super::*;
use crate::org::mint_operation;
use crate::testkit::{OrgFixture, ScriptSpec, serve_one};
use phux_protocol::caps::{
    ServerFeatureExt, ServerFeatureExt2, ServerFeatureExt2Set, ServerFeatureExtSet,
};
use phux_protocol::wire::frame::{CommandResult, ExecState, FrameKind, Refusal};

fn spec(fixture: OrgFixture, ensure: bool) -> ScriptSpec {
    let ext2 = if ensure {
        ServerFeatureExt2Set::all()
    } else {
        ServerFeatureExt2Set::with(&[ServerFeatureExt2::ArchivedLaunchAdmission])
    };
    ScriptSpec::new()
        .organization(fixture)
        .server_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
        .server_features_ext2(ext2)
}

#[tokio::test]
async fn create_collision_refuses_while_ensure_existing_preserves_execution() {
    let root = tempfile::tempdir().unwrap();
    let mut fixture = OrgFixture::new();
    let session = fixture.session("work", SessionLifetime::Persistent);
    fixture.resource(session, Some(ExecState::Running));
    let (socket, server) = serve_one(root.path(), spec(fixture, true));
    let mut conn = Connection::connect(&socket).await.unwrap();
    assert!(matches!(
        create_session(
            &mut conn,
            mint_operation(),
            "work".into(),
            SessionLifetime::Persistent,
            None
        )
        .await,
        Err(OrgError::Refused(Refusal::NameTaken))
    ));
    let receipt = ensure_session(
        &mut conn,
        mint_operation(),
        "work".into(),
        SessionLifetime::Ephemeral,
        Some(Box::new(LaunchSpec::new(vec!["must-not-run".into()]))),
        Vec::new(),
    )
    .await
    .unwrap();
    assert_eq!(receipt.session, Some(session));
    assert_eq!(receipt.ensured_created, Some(false));
    let inventory = fetch(&mut conn, OrgFilter::Session(session), 100)
        .await
        .unwrap();
    assert_eq!(
        inventory.resources.len(),
        1,
        "ensure(existing) cannot launch a second resource"
    );
    assert_eq!(
        inventory.session(&session).unwrap().lifetime,
        SessionLifetime::Persistent
    );
    assert!(matches!(
        attach_target_for_receipt(&mut conn, &receipt)
            .await
            .unwrap(),
        AttachTarget::ById(_)
    ));
    drop(conn);
    let frames = server.await.unwrap();
    assert!(!frames.iter().any(|frame| matches!(
        frame,
        FrameKind::Attach { .. } | FrameKind::SpawnResource { .. }
    )));
}

#[tokio::test]
async fn ensure_replay_and_attach_follow_original_key_after_name_reuse() {
    let root = tempfile::tempdir().unwrap();
    let (socket, server) = serve_one(root.path(), spec(OrgFixture::new(), true));
    let mut conn = Connection::connect(&socket).await.unwrap();
    let operation = mint_operation();
    let receipt = ensure_session(
        &mut conn,
        operation,
        "work".into(),
        SessionLifetime::Persistent,
        None,
        Vec::new(),
    )
    .await
    .unwrap();
    assert_eq!(receipt.ensured_created, Some(true));
    let session = receipt.session.unwrap();
    let original_target = attach_target_for_receipt(&mut conn, &receipt)
        .await
        .unwrap();
    let mut foreign = receipt.clone();
    foreign.authority = phux_protocol::ids::AuthorityId::new([0x55; 16]);
    assert!(matches!(
        attach_target_for_receipt(&mut conn, &foreign).await,
        Err(OrgError::NotFound(_))
    ));
    commit(
        &mut conn,
        101,
        Command::SessionRename {
            operation: mint_operation(),
            session,
            name: "renamed".into(),
        },
    )
    .await
    .unwrap();
    let replacement = create_session(
        &mut conn,
        mint_operation(),
        "work".into(),
        SessionLifetime::Persistent,
        None,
    )
    .await
    .unwrap();
    assert_ne!(replacement.session, receipt.session);
    let replay = ensure_session(
        &mut conn,
        operation,
        "work".into(),
        SessionLifetime::Persistent,
        None,
        Vec::new(),
    )
    .await
    .unwrap();
    assert_eq!(replay.session, receipt.session);
    assert_eq!(replay.ensured_created, Some(true));
    assert_eq!(
        attach_target_for_receipt(&mut conn, &receipt)
            .await
            .unwrap(),
        original_target
    );
    drop(conn);
    server.await.unwrap();
}

#[tokio::test]
async fn unsupported_ensure_accepts_no_work_and_unreadable_receipt_retains_original_operation() {
    for supported in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let input = spec(OrgFixture::new(), supported).mutation_reply(CommandResult::Ok);
        let (socket, server) = serve_one(root.path(), input);
        let mut conn = Connection::connect(&socket).await.unwrap();
        let operation = mint_operation();
        let result = ensure_session(
            &mut conn,
            operation,
            "new".into(),
            SessionLifetime::Persistent,
            None,
            Vec::new(),
        )
        .await;
        if supported {
            assert!(
                matches!(result, Err(OrgError::OutcomeUnknown { operation: Some(actual) }) if actual == operation)
            );
        } else {
            assert!(matches!(result, Err(OrgError::Unsupported)));
        }
        drop(conn);
        let frames = server.await.unwrap();
        let mutations: Vec<_> = frames
            .iter()
            .filter_map(|frame| match frame {
                FrameKind::Command { command, .. } if command.idempotency_key().is_some() => {
                    Some(command)
                }
                _ => None,
            })
            .collect();
        assert_eq!(mutations.len(), usize::from(supported));
        assert!(mutations.iter().all(|command| matches!(command, Command::SessionEnsure { operation: actual, .. } if *actual == operation)));
    }
}

#[tokio::test]
async fn ensure_applies_attributes_only_on_create_and_receipt_retry_binds_exact_payload() {
    let root = tempfile::tempdir().unwrap();
    let (socket, server) = serve_one(root.path(), spec(OrgFixture::new(), true));
    let mut conn = Connection::connect(&socket).await.unwrap();
    let operation = mint_operation();
    let attrs = vec![("purpose".into(), "original".into())];
    let created = ensure_session(
        &mut conn,
        operation,
        "work".into(),
        SessionLifetime::Persistent,
        None,
        attrs.clone(),
    )
    .await
    .unwrap();
    let session = created.session.unwrap();
    let existing = ensure_session(
        &mut conn,
        mint_operation(),
        "work".into(),
        SessionLifetime::Persistent,
        None,
        vec![("purpose".into(), "replacement".into())],
    )
    .await
    .unwrap();
    assert_eq!(existing.ensured_created, Some(false));
    let inventory = fetch(&mut conn, OrgFilter::Session(session), 102)
        .await
        .unwrap();
    assert_eq!(inventory.session(&session).unwrap().attrs, attrs);
    assert!(matches!(
        ensure_session(
            &mut conn,
            operation,
            "work".into(),
            SessionLifetime::Persistent,
            None,
            vec![("purpose".into(), "changed retry".into())]
        )
        .await,
        Err(OrgError::Refused(Refusal::DuplicateOperation))
    ));
    drop(conn);
    server.await.unwrap();
}
