//! Typed ingress regressions use only an explicit temporary mock socket.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use super::*;
use crate::testkit::{ScriptSpec, ScriptedServer};
use phux_protocol::caps::ServerFeatureExtSet;
use tokio::net::UnixListener;

fn command() -> NativeCommand {
    NativeCommand {
        operation_id: u64::MAX - 1,
        activation: "activation-A".into(),
        intent: phux_protocol::native_agent::NativeIntent::Prompt {
            text: "one prompt".into(),
        },
    }
}

fn receipt(operation_id: u64, disposition: NativeDisposition) -> CommandResult {
    CommandResult::OkWith(CommandValue::Json(
        serde_json::to_string(&NativeCommandResult {
            operation_id,
            disposition,
            detail: None,
        })
        .unwrap(),
    ))
}

fn event(agent: u32, operation_id: u64, disposition: NativeDisposition) -> FrameKind {
    FrameKind::Event {
        terminal: Some(ResourceId::local(agent)),
        event: AgentEvent::NativeAgentResult {
            agent_session_id: ResourceId::local(agent),
            operation_id,
            disposition,
            detail: None,
        },
        stamp: None,
    }
}

#[test]
fn receipts_preserve_queued_and_require_exact_operation_identity() {
    let id = command().operation_id;
    assert_eq!(
        command_receipt(receipt(id, NativeDisposition::Queued), id).disposition,
        NativeDisposition::Queued
    );
    assert_eq!(
        command_receipt(receipt(id - 1, NativeDisposition::Completed), id).disposition,
        NativeDisposition::Unknown
    );
    assert_eq!(
        command_receipt(CommandResult::Ok, id).disposition,
        NativeDisposition::Unknown
    );
}

#[test]
fn events_require_both_agent_and_full_width_operation_identity() {
    let id = command().operation_id;
    assert!(native_result(event(23, id, NativeDisposition::Completed), 22, id).is_none());
    assert!(native_result(event(22, id - 1, NativeDisposition::Completed), 22, id).is_none());
    let matched = native_result(event(22, id, NativeDisposition::Completed), 22, id).unwrap();
    assert_eq!(matched.operation_id, id);
    assert_eq!(matched.disposition, NativeDisposition::Completed);
}

#[test]
fn explicit_refusal_is_not_uncertain_delivery() {
    let id = command().operation_id;
    for code in [ErrorCode::PermissionDenied, ErrorCode::PreconditionFailed] {
        let result = command_receipt(
            CommandResult::Error {
                code,
                message: "refused".into(),
            },
            id,
        );
        assert_eq!(result.disposition, NativeDisposition::Refused);
        assert_eq!(result.detail.as_deref(), Some("refused"));
    }
    for code in [
        ErrorCode::InputDeliveryUnknown,
        ErrorCode::IncarnationChanged,
        ErrorCode::InternalError,
    ] {
        let result = command_receipt(
            CommandResult::Error {
                code,
                message: "uncertain".into(),
            },
            id,
        );
        assert_eq!(result.disposition, NativeDisposition::Unknown);
    }
}

fn mock(spec: ScriptSpec) -> (tempfile::TempDir, tokio::task::JoinHandle<Vec<FrameKind>>) {
    let dir = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(dir.path().join("native.sock")).unwrap();
    let task = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });
    (dir, task)
}

#[tokio::test]
async fn legacy_peer_gets_no_command_or_subscription() {
    let (dir, task) = mock(ScriptSpec::new());
    let result = submit_command(&dir.path().join("native.sock"), 22, command())
        .await
        .unwrap();
    assert_eq!(result.disposition, NativeDisposition::Refused);
    assert_eq!(result.operation_id, command().operation_id);
    let seen = task.await.unwrap();
    assert!(!seen.iter().any(|frame| matches!(
        frame,
        FrameKind::Command { .. } | FrameKind::SubscribeEvents { .. }
    )));
}

#[tokio::test]
async fn missing_receipt_is_unknown_and_command_is_never_replayed() {
    let spec = ScriptSpec::new()
        .server_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::NativeAgent]));
    let (dir, task) = mock(spec);
    let result = submit_command(&dir.path().join("native.sock"), 22, command())
        .await
        .unwrap();
    assert_eq!(result.disposition, NativeDisposition::Unknown);
    assert_eq!(result.operation_id, command().operation_id);
    assert!(
        result
            .detail
            .unwrap()
            .contains("do not send a replacement intent")
    );
    let seen = task.await.unwrap();
    let requests: Vec<_> = seen
        .iter()
        .filter_map(|frame| match frame {
            FrameKind::Command {
                command: Command::NativeAgent { request },
                ..
            } => Some(request.request()),
            _ => None,
        })
        .collect();
    assert_eq!(requests.len(), 1);
    assert!(
        matches!(requests[0], NativeRequest::Command { agent_session_id, command: sent } if *agent_session_id == ResourceId::local(22) && *sent == command())
    );
}

#[tokio::test]
async fn legacy_peer_gets_no_spawn() {
    let (dir, task) = mock(ScriptSpec::new());
    let spec = SpawnNativeAgent {
        session: "work".into(),
        backend: "acp".into(),
        provider: "fake".into(),
        command: vec!["fake-acp".into()],
        cwd: None,
        env: std::collections::BTreeMap::new(),
        native_id: None,
        guardian: None,
        guardian_key: None,
        guardian_expected: None,
    };
    assert!(matches!(
        spawn(&dir.path().join("native.sock"), spec).await,
        Err(ClientError::Transport(AttachError::Protocol(_)))
    ));
    assert!(
        !task
            .await
            .unwrap()
            .iter()
            .any(|frame| matches!(frame, FrameKind::Command { .. }))
    );
}

fn snapshot() -> SessionSnapshot {
    use phux_protocol::ids::{SessionId, WindowId};
    let mut snapshot = SessionSnapshot::new(SessionId(1), WindowId(1), ResourceId::local(22));
    snapshot
        .resources
        .push(phux_protocol::wire::info::ResourceInfo::new(
            ResourceId::local(22),
            WindowId(1),
            80,
            24,
        ));
    snapshot
}

#[tokio::test]
async fn recipe_degradation_is_delivered_even_before_decode_or_refusal_failure() {
    for spec in [
        ScriptSpec::new().metadata(|_, _| Some(b"not JSON".to_vec())),
        ScriptSpec::new().refuse_metadata(ErrorCode::PermissionDenied, "private recipe"),
    ] {
        let (dir, task) = mock(spec.degradation_notice("satellite unavailable"));
        let mut notices = Vec::new();
        let result = fetch_launch_recipes(
            &dir.path().join("native.sock"),
            &snapshot(),
            |degradation| {
                notices.extend_from_slice(degradation.notices());
            },
        )
        .await;
        assert!(matches!(result, Err(ClientError::Protocol(_))));
        assert_eq!(notices, ["satellite unavailable"]);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn recipe_absence_is_not_an_error_and_preserves_degradation() {
    let (dir, task) = mock(ScriptSpec::new().degradation_notice("satellite unavailable"));
    let mut notices = Vec::new();
    let recipes = fetch_launch_recipes(
        &dir.path().join("native.sock"),
        &snapshot(),
        |degradation| {
            notices.extend_from_slice(degradation.notices());
        },
    )
    .await
    .unwrap();
    assert!(recipes.is_empty());
    assert_eq!(notices, ["satellite unavailable"]);
    task.await.unwrap();
}
