//! Worktree open crosses the same atomic ensure and durable receipt seam as CLI attach.
use super::*;
use crate::commands::new::workflow_tests::{Fixture, Reply};
use phux_protocol::wire::frame::{AttachTarget, Command};

#[tokio::test]
async fn open_ensures_existing_or_missing_then_uses_only_the_receipt_session() {
    for reply in [Reply::Existing, Reply::Created, Reply::ReusedName] {
        let fixture = Fixture::start(reply);
        let server = ServerTarget::Local(fixture.socket.clone());
        let (mut conn, receipt) = ensure_open_session(&server, "work", Path::new("/work"))
            .await
            .unwrap();
        assert_eq!(
            seed_for_receipt(&mut conn, &receipt).await.unwrap(),
            Some(4)
        );
        drop(conn);
        let commands = fixture.finish().await;
        assert_eq!(commands.len(), 2);
        assert!(
            matches!(&commands[0], Command::SessionEnsure { lifetime: SessionLifetime::Persistent, launch: Some(launch), .. } if launch.command.is_empty() && launch.cwd.as_deref() == Some("/work"))
        );
        assert!(!commands.iter().any(|command| matches!(
            command,
            Command::SessionCreate { .. } | Command::ResourceCreate { .. }
        )));
    }
}

#[tokio::test]
async fn attached_open_selects_original_receipt_key_even_after_name_reuse() {
    let fixture = Fixture::start(Reply::ReusedName);
    let server = ServerTarget::Local(fixture.socket.clone());
    let (mut conn, receipt) = ensure_open_session(&server, "work", Path::new("/work"))
        .await
        .unwrap();
    assert_eq!(
        org::attach_target_for_receipt(&mut conn, &receipt)
            .await
            .unwrap(),
        AttachTarget::ById(phux_protocol::ids::SessionId::new(7))
    );
    drop(conn);
    assert_eq!(fixture.finish().await.len(), 2);
}

#[tokio::test]
async fn open_preserves_uncertain_operation_and_does_not_fallback_to_create() {
    for reply in [
        Reply::Lost,
        Reply::Unknown,
        Reply::MissingIdentity,
        Reply::MissingEnsureOutcome,
    ] {
        let fixture = Fixture::start(reply);
        let server = ServerTarget::Local(fixture.socket.clone());
        let result = ensure_open_session(&server, "work", Path::new("/work")).await;
        let Err(OrgError::OutcomeUnknown {
            operation: Some(operation),
        }) = result
        else {
            panic!("ensure requires the original operation after an uncertain receipt");
        };
        let commands = fixture.finish().await;
        assert_eq!(commands.len(), 1);
        assert!(
            matches!(&commands[0], Command::SessionEnsure { operation: original, .. } if *original == operation)
        );
    }
}

#[tokio::test]
async fn old_peer_does_not_accept_worktree_creation_before_unsupported() {
    for reply in [Reply::Legacy, Reply::NoEnsure] {
        let fixture = Fixture::start(reply);
        let server = ServerTarget::Local(fixture.socket.clone());
        let result = ensure_open_session(&server, "work", Path::new("/work")).await;
        assert!(matches!(result, Err(OrgError::Unsupported)));
        assert!(fixture.finish().await.is_empty());
    }
}

#[tokio::test]
async fn removed_session_cannot_report_the_replacement_names_pane() {
    let fixture = Fixture::start(Reply::Removed);
    let server = ServerTarget::Local(fixture.socket.clone());
    let (mut conn, receipt) = ensure_open_session(&server, "work", Path::new("/work"))
        .await
        .unwrap();
    assert_eq!(seed_for_receipt(&mut conn, &receipt).await.unwrap(), None);
    let error = new::committed_session_error("work", &receipt);
    let doc = phux_client::contract::error_document(&error, 1);
    assert_eq!(doc["error"]["recovery"]["effect_certainty"], "applied");
    assert_eq!(doc["error"]["recovery"]["next_action"], "reconcile");
    drop(conn);
    assert_eq!(fixture.finish().await.len(), 2);
}

#[tokio::test]
async fn worktree_ensure_carries_project_identity_in_the_atomic_request() {
    let fixture = Fixture::start(Reply::Created);
    let checkout = tempfile::tempdir().unwrap();
    std::fs::create_dir(checkout.path().join(".git")).unwrap();
    std::fs::write(checkout.path().join(".git/HEAD"), "ref: refs/heads/work\n").unwrap();
    std::fs::write(
        checkout.path().join(".git/config"),
        "[remote \"origin\"]\nurl = git@github.com:team/work.git\n",
    )
    .unwrap();
    let expected = phux_client::project_identity::creation_attrs(
        checkout.path(),
        &crate::commands::ls::local_hostname(),
        None,
    );
    assert!(
        !expected.is_empty(),
        "fixture must exercise nonempty atomic project attributes"
    );
    let server = ServerTarget::Local(fixture.socket.clone());
    let (conn, _) = ensure_open_session(&server, "work", checkout.path())
        .await
        .unwrap();
    drop(conn);
    let commands = fixture.finish().await;
    assert_eq!(commands.len(), 1);
    let Command::SessionEnsure { attrs, .. } = &commands[0] else {
        panic!("atomic ensure only");
    };
    assert_eq!(*attrs, expected);
}
