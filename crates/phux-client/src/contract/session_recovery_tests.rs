use super::*;
use phux_protocol::ids::{OperationId, SessionKey};

#[test]
fn uncertain_session_attach_error_retains_original_operation_and_blocks_transport_retry() {
    let operation = OperationId::new([7; 16]).unwrap();
    let error = AttachError::OperationOutcomeUnknown { operation: Some(operation) };
    assert!(!error.is_transport_loss());
    let refusal = transport_refusal(&error, Path::new("/isolated/server.sock"), "new", None);
    let document = error_document(&refusal, 1);
    assert_eq!(document["error"]["code"], "outcome_unknown");
    assert_eq!(document["error"]["recovery"]["operation_id"], operation.to_hex());
    assert_eq!(document["error"]["recovery"]["effect_certainty"], "unknown");
    assert_eq!(document["error"]["recovery"]["next_action"], "reconcile");
}

#[test]
fn failed_selection_after_commit_keeps_applied_intent_and_exact_session_identity() {
    let operation = OperationId::new([8; 16]).unwrap();
    let session = SessionKey::new([9; 16]).unwrap();
    let error = AttachError::CommittedSelectionUnavailable { operation, session, message: "live id unavailable".into() };
    assert!(!error.is_transport_loss());
    let refusal = transport_refusal(&error, Path::new("/isolated/server.sock"), "new", None);
    let document = error_document(&refusal, 1);
    assert_eq!(document["error"]["recovery"]["operation_id"], operation.to_hex());
    assert_eq!(document["error"]["recovery"]["effect_certainty"], "applied");
    assert_eq!(document["error"]["recovery"]["next_action"], "reconcile");
    assert!(document["error"]["message"].as_str().unwrap().contains(&session.to_string()));
}
