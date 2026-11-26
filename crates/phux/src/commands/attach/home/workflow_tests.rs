//! Exercise the configured attach CLI workflow, not a second ensure policy.
use phux_client::attach::AttachError;
use phux_protocol::ids::SessionId;
use phux_protocol::wire::frame::{AttachTarget, Command, SessionLifetime};

use crate::commands::new::workflow_tests::{Fixture, Reply};

fn configured() -> super::ConfiguredSession {
    let defaults = phux_config::DefaultsCfg {
        attach_session: Some("work".into()),
        ..phux_config::DefaultsCfg::default()
    };
    super::target_for(&defaults, Some("/work".into())).unwrap()
}

#[tokio::test]
async fn configured_attach_ensures_existing_and_missing_without_extra_launch() {
    for reply in [Reply::Existing, Reply::Created, Reply::ReusedName] {
        let fixture = Fixture::start(reply);
        let (conn, target) = configured()
            .ensure_target(&fixture.dial(), None)
            .await
            .unwrap();
        assert_eq!(target, AttachTarget::ById(SessionId::new(7)));
        drop(conn);
        let commands = fixture.finish().await;
        assert_eq!(
            commands.len(),
            2,
            "ensure then exact identity read, no extra pane"
        );
        assert!(
            matches!(&commands[0], Command::SessionEnsure { lifetime: SessionLifetime::Ephemeral, launch: Some(launch), .. } if launch.command.is_empty() && launch.cwd.as_deref() == Some("/work"))
        );
        assert!(matches!(&commands[1], Command::GetOrganization { .. }));
    }
}

#[tokio::test]
async fn configured_attach_on_older_peer_accepts_no_hidden_creation() {
    for reply in [Reply::Legacy, Reply::NoEnsure] {
        let fixture = Fixture::start(reply);
        let result = configured().ensure_target(&fixture.dial(), None).await;
        assert!(matches!(result, Err(AttachError::Refused(_))));
        assert!(fixture.finish().await.is_empty());
    }
}

#[tokio::test]
async fn lost_or_untrustworthy_ensure_receipt_keeps_original_operation_without_retry() {
    for reply in [
        Reply::Lost,
        Reply::Unknown,
        Reply::MissingIdentity,
        Reply::MissingEnsureOutcome,
    ] {
        let fixture = Fixture::start(reply);
        let result = configured().ensure_target(&fixture.dial(), None).await;
        let Err(AttachError::OperationOutcomeUnknown {
            operation: Some(operation),
        }) = result
        else {
            panic!("missing receipt must preserve unknown outcome and original operation");
        };
        let commands = fixture.finish().await;
        assert_eq!(commands.len(), 1);
        assert!(
            matches!(&commands[0], Command::SessionEnsure { operation: original, .. } if *original == operation)
        );
    }
}

#[tokio::test]
async fn removed_receipt_identity_cannot_select_a_replacement_with_the_same_name() {
    let fixture = Fixture::start(Reply::Removed);
    let result = configured().ensure_target(&fixture.dial(), None).await;
    assert!(matches!(
        result,
        Err(AttachError::CommittedSelectionUnavailable { .. })
    ));
    let commands = fixture.finish().await;
    assert_eq!(commands.len(), 2);
    assert!(matches!(&commands[0], Command::SessionEnsure { .. }));
    assert!(matches!(&commands[1], Command::GetOrganization { .. }));
}

#[tokio::test]
async fn startup_disposition_preserves_first_server_command_without_launching_on_reuse() {
    use crate::commands::server::EnsureDisposition;
    for disposition in [
        EnsureDisposition::DaemonStarted,
        EnsureDisposition::SupervisedStarted,
        EnsureDisposition::Reused,
        EnsureDisposition::Joined,
    ] {
        let fixture = Fixture::start(Reply::Created);
        let defaults = phux_config::DefaultsCfg {
            attach_session: Some("work".into()),
            spawn_on_attach: Some("exec htop".into()),
            shell: Some("/bin/sh".into()),
            ..phux_config::DefaultsCfg::default()
        };
        let configured = super::target_for(&defaults, None).unwrap();
        let (conn, _) = configured
            .ensure_target(&fixture.dial(), Some(disposition))
            .await
            .unwrap();
        drop(conn);
        let commands = fixture.finish().await;
        assert_eq!(commands.len(), 2);
        let Command::SessionEnsure {
            launch: Some(launch),
            ..
        } = &commands[0]
        else {
            panic!("startup only projects a seed into the ensure");
        };
        match disposition {
            EnsureDisposition::Reused | EnsureDisposition::Joined => {
                assert!(launch.command.is_empty());
            }
            EnsureDisposition::SupervisedStarted => {
                assert_eq!(launch.command, ["/bin/sh", "-l", "-c", "exec htop"]);
            }
            EnsureDisposition::DaemonStarted => {
                assert_eq!(launch.command.first().map(String::as_str), Some("/bin/sh"));
                assert!(
                    launch
                        .command
                        .ends_with(&["-c".to_owned(), "exec htop".to_owned()])
                );
            }
        }
    }
}
