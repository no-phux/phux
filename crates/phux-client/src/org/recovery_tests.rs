use super::*;
use phux_protocol::ids::Revision;
use phux_protocol::wire::frame::{CommandResult, CommandValue};

fn key(byte: u8) -> OperationId {
    OperationId::new([byte; 16]).unwrap()
}

#[test]
fn foreign_commit_keeps_original_recovery_identity_instead_of_false_success() {
    let original = key(1);
    let result = CommandResult::OkWith(CommandValue::Committed(Box::new(Committed::new(
        key(2),
        Revision::new(1),
    ))));
    let error = checked_outcome(&result, Some(original)).unwrap_err();
    assert!(matches!(error, OrgError::OutcomeUnknown { operation: Some(id) } if id == original));
    assert_eq!(error.recovery().operation_id, Some(original.to_hex()));
}

#[test]
fn unreadable_mutation_reply_preserves_identity_and_uncertainty() {
    let original = key(3);
    let result = CommandResult::Error {
        code: phux_protocol::wire::frame::ErrorCode::Unknown(65535),
        message: "unreadable reply".into(),
    };
    let error = checked_outcome(&result, Some(original)).unwrap_err();
    assert_eq!(error.code(), "outcome_unknown");
    assert_eq!(error.recovery().operation_id, Some(original.to_hex()));
}

#[test]
fn matching_commit_remains_successful_and_read_only_errors_are_not_reclassified() {
    let original = key(4);
    let result = CommandResult::OkWith(CommandValue::Committed(Box::new(Committed::new(
        original,
        Revision::new(1),
    ))));
    assert!(matches!(
        checked_outcome(&result, Some(original)).unwrap(),
        OrgOutcome::Committed(_)
    ));
    let unreadable = CommandResult::Error {
        code: phux_protocol::wire::frame::ErrorCode::Unknown(65535),
        message: "unreadable read".into(),
    };
    assert!(matches!(
        checked_outcome(&unreadable, None).unwrap(),
        OrgOutcome::Unknown { .. }
    ));
    let value = CommandResult::OkWith(CommandValue::Json("read result".into()));
    assert!(matches!(
        checked_outcome(&value, None).unwrap(),
        OrgOutcome::Value(_)
    ));
}

#[derive(Clone, Copy)]
enum Caller {
    Request,
    Commit,
    Create,
}

async fn rejected_payload(
    caller: Caller,
    result: CommandResult,
    operation: OperationId,
) -> OrgError {
    use crate::session::{CreateSessionError, CreateSessionRequest};
    use crate::testkit::{ScriptSpec, serve_one};
    use phux_protocol::caps::ServerFeatureExtSet;
    use phux_protocol::wire::frame::SessionLifetime;
    let root = tempfile::tempdir().unwrap();
    let spec = ScriptSpec::new()
        .server_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
        .mutation_reply(result);
    let (socket, server) = serve_one(root.path(), spec);
    let mut conn = Connection::connect(&socket).await.unwrap();
    let command = || Command::SessionCreate {
        operation,
        name: "recovery".into(),
        lifetime: SessionLifetime::Persistent,
        attrs: vec![],
        launch: None,
    };
    let error = match caller {
        Caller::Request => request(&mut conn, 1, command()).await.unwrap_err(),
        Caller::Commit => commit(&mut conn, 1, command()).await.unwrap_err(),
        Caller::Create => {
            let env = std::collections::BTreeMap::new();
            let input = CreateSessionRequest {
                name: "recovery",
                command: None,
                cwd: None,
                env: &env,
                agent_session: None,
                idempotency_key: Some(operation),
                lifetime: SessionLifetime::Persistent,
                attrs: &[],
            };
            let error = crate::session::create_session(&mut conn, &input, &mut Vec::new())
                .await
                .unwrap_err();
            let CreateSessionError::Org(error) = error else {
                panic!("organization uncertainty");
            };
            error
        }
    };
    drop(conn);
    server.await.unwrap();
    error
}

#[tokio::test]
async fn foreign_shaped_payload_stays_unknown_through_request_commit_and_session_create() {
    let operation = key(8);
    for caller in [Caller::Request, Caller::Commit, Caller::Create] {
        let error = rejected_payload(
            caller,
            CommandResult::OkWith(CommandValue::Json("unrelated payload".into())),
            operation,
        )
        .await;
        assert!(
            matches!(error, OrgError::OutcomeUnknown { operation: Some(id) } if id == operation)
        );
        assert_eq!(error.recovery().operation_id, Some(operation.to_hex()));
    }
}

#[tokio::test]
async fn bare_acceptance_without_creation_receipt_retains_original_identity() {
    let operation = key(9);
    for caller in [Caller::Commit, Caller::Create] {
        let error = rejected_payload(caller, CommandResult::Ok, operation).await;
        assert_eq!(error.code(), "outcome_unknown");
        assert_eq!(error.recovery().operation_id, Some(operation.to_hex()));
    }
}
