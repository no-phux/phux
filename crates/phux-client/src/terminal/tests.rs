//! Unexpected receipt-read replies are unknown, never evidence of commitment.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;

#[test]
fn another_operations_valid_receipt_cannot_acknowledge_this_intention() {
    use phux_protocol::ids::{ArrangementKey, AuthorityId, OperationId, ResourceKey, Revision};
    let mut receipt = Committed::new(OperationId::new([0xab; 16]).unwrap(), Revision::new(1))
        .with_creation(
            Some(AuthorityId::new([2; 16]).unwrap()),
            Some(ArrangementKey::new([4; 16]).unwrap()),
            Some([5; 16]),
            None,
        );
    receipt.resource = Some(ResourceKey::new([3; 16]).unwrap());
    let session = "aebagbafaydqqcikbmga2dqpca";
    let original = api::hex16(receipt.operation.as_bytes());
    assert!(project(session, &original.to_ascii_uppercase(), &receipt).is_ok());
    let requested = api::hex16(&[0xac; 16]);
    let error = project(session, &requested, &receipt).unwrap_err();
    assert_eq!(error.document(&requested)["error"]["outcome"], "unknown");
}
#[tokio::test]
async fn failed_lookup_connection_leaves_original_outcome_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let request = api::TerminalOperation {
        idempotency_key: "abababababababababababababababab".into(),
        session: "aebagbafaydqqcikbmga2dqpca".into(),
    };
    let error = operation_on(&dir.path().join("absent.sock"), &request)
        .await
        .unwrap_err();
    assert_eq!(
        error.document(&request.idempotency_key)["error"]["outcome"],
        "unknown"
    );
}

#[tokio::test]
async fn unreadable_lookup_does_not_claim_original_intent_committed() {
    use crate::testkit::{ScriptSpec, serve_one};
    use phux_protocol::caps::ServerFeatureExtSet;
    let dir = tempfile::tempdir().unwrap();
    // No organization fixture: this peer answers the read with a bare OK.
    let spec = ScriptSpec::new().server_features_ext(ServerFeatureExtSet::with(&[
        ServerFeatureExt::Organization,
        ServerFeatureExt::ApiDescription,
    ]));
    let (socket, server) = serve_one(dir.path(), spec);
    let request = api::TerminalOperation {
        idempotency_key: "abababababababababababababababab".into(),
        session: "aebagbafaydqqcikbmga2dqpca".into(),
    };
    let error = operation_on(&socket, &request).await.unwrap_err();
    assert_eq!(
        error.document(&request.idempotency_key)["error"]["outcome"],
        "unknown"
    );
    let seen = server.await.unwrap();
    assert!(seen.iter().all(|frame| !matches!(
        frame,
        phux_protocol::wire::frame::FrameKind::Command {
            command: Command::ResourceCreate { .. },
            ..
        }
    )));
}
