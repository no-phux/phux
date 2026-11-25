use super::*;
use phux_protocol::ids::{ActivationId, ResourceKey, Revision, ServerInstance, SessionKey};
use phux_protocol::wire::frame::{CommandResult, CommandValue, Committed, LiveTarget};

#[tokio::test]
async fn committed_create_without_live_target_retains_exact_receipt_instead_of_not_registered() {
    let operation = IdempotencyKey::new([0x21; 16]).unwrap();
    let session = SessionKey::new([0x22; 16]).unwrap();
    let receipt = Committed::new(operation, Revision::new(1)).with_session(Some(session));
    let spec = ScriptSpec::new().mutation_reply(CommandResult::OkWith(CommandValue::Committed(Box::new(receipt.clone()))));
    let (socket, _dir, server) = scripted(spec);
    let mut conn = Connection::connect(&socket).await.unwrap();
    let env = BTreeMap::new();
    let mut request = work_request(&env);
    request.idempotency_key = Some(operation);
    let outcome = create_session(&mut conn, &request, &mut Vec::new()).await.unwrap();
    assert!(matches!(outcome, CreateOutcome::Committed { receipt: actual } if *actual == receipt));
    drop(conn);
    let frames = server.await.unwrap();
    assert_eq!(frames.iter().filter(|frame| matches!(frame, FrameKind::Command { command: Command::SessionCreate { .. }, .. })).count(), 1);
    assert!(!frames.iter().any(|frame| matches!(frame, FrameKind::Attach { .. } | FrameKind::SpawnResource { .. })));
}

#[tokio::test]
async fn failed_post_commit_inspection_retains_admission_and_does_not_mutate_arrangement() {
    let operation = IdempotencyKey::new([0x31; 16]).unwrap();
    let receipt = Committed::new(operation, Revision::new(1))
        .with_session(Some(SessionKey::new([0x32; 16]).unwrap()))
        .with_resource(Some(ResourceKey::new([0x33; 16]).unwrap()))
        .with_target(Some(LiveTarget { resource: phux_protocol::ResourceId::local(1),
            instance: ServerInstance::new([0x34; 16]), activation: ActivationId::new(1) }));
    // Bare GET_ORGANIZATION gets an unreadable-for-that-read Ok reply; the
    // mutation receipt above already proves admission and cannot be forgotten.
    let spec = ScriptSpec::new().mutation_reply(CommandResult::OkWith(CommandValue::Committed(Box::new(receipt.clone()))));
    let (socket, _dir, server) = scripted(spec);
    let mut conn = Connection::connect(&socket).await.unwrap();
    let env = BTreeMap::new();
    let mut request = work_request(&env);
    request.idempotency_key = Some(operation);
    let outcome = create_session(&mut conn, &request, &mut Vec::new()).await.unwrap();
    assert!(matches!(outcome, CreateOutcome::Committed { receipt: actual } if *actual == receipt));
    drop(conn);
    let frames = server.await.unwrap();
    let mutations: Vec<_> = frames.iter().filter_map(|frame| match frame {
        FrameKind::Command { command, .. } if command.idempotency_key().is_some() => Some(command), _ => None,
    }).collect();
    assert_eq!(mutations.len(), 1, "post-commit observation never authorizes another edit");
    assert!(matches!(mutations[0], Command::SessionCreate { operation: actual, .. } if *actual == operation));
}
