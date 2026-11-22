use super::*;
use phux_protocol::api::{ErrorCode, ErrorDocument, OperationOutcome};
use std::process::Output;

const KEY: &str = "abababababababababababababababab";

fn failure(
    output: &Output,
    status: i32,
    code: ErrorCode,
    outcome: OperationOutcome,
) -> ErrorDocument {
    assert_eq!(output.status.code(), Some(status), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let document: ErrorDocument = serde_json::from_slice(&output.stderr)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stderr)));
    assert_eq!(document.schema_version, 1);
    assert_eq!(document.error.code, code, "{document:?}");
    assert_eq!(document.error.outcome, outcome, "{document:?}");
    document
}

#[test]
fn api_target_refusals_keep_the_request_identity() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("absent.sock");
    let request = json!({"session":SESSION, "idempotency_key":KEY}).to_string();
    for (operation, outcome) in [
        ("terminal.create", OperationOutcome::NotSent),
        ("terminal.operation", OperationOutcome::Unknown),
    ] {
        let output = common::phux_cmd(runner::phux_bin())
            .args([
                "api",
                "--remote",
                "no-enrollment.invalid",
                "call",
                operation,
                "--request-json",
                &request,
            ])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let error = failure(&output, 1, ErrorCode::Transport, outcome);
        assert!(error.error.message.contains("not a registered host"));
        assert_eq!(error.error.idempotency_key.as_deref(), Some(KEY));

        let output = common::phux_cmd(runner::phux_bin())
            .arg("--socket")
            .arg(&socket)
            .args([
                "api",
                "--remote",
                "no-enrollment.invalid",
                "call",
                operation,
                "--request-json",
                &request,
            ])
            .output()
            .unwrap();
        let error = failure(&output, 2, ErrorCode::InvalidArgument, outcome);
        assert!(error.error.message.contains("cannot combine"));
        assert_eq!(error.error.idempotency_key.as_deref(), Some(KEY));
    }

    let output = common::phux_cmd(runner::phux_bin())
        .args(["api", "--remote", "quic://bad:8788", "list"])
        .output()
        .unwrap();
    failure(
        &output,
        2,
        ErrorCode::InvalidArgument,
        OperationOutcome::NotSent,
    );
}

#[test]
fn api_rejects_malformed_and_semantically_invalid_requests() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("absent.sock");
    let semantic = json!({"session":SESSION, "idempotency_key":KEY, "cwd":"relative"}).to_string();
    for (input, status, key) in [("{", 2, None), (semantic.as_str(), 1, Some(KEY))] {
        let output = common::phux_cmd(runner::phux_bin())
            .arg("--socket")
            .arg(&socket)
            .args(["api", "call", "terminal.create", "--request-json", input])
            .output()
            .unwrap();
        let error = failure(
            &output,
            status,
            ErrorCode::InvalidArgument,
            OperationOutcome::NotSent,
        );
        assert_eq!(error.error.idempotency_key.as_deref(), key);
    }
}

#[test]
fn question_receipt_target_failure_preserves_its_operation_id() {
    let request = json!({
        "operation_id": "42",
        "question": {
            "authority": "aebagbafaydqqcikbmga2dqpca",
            "conversation": "aebagbafaydqqcikbmga2dqpca",
            "activation": "activation",
            "id": "callback",
            "revision": "ab".repeat(32),
        }
    });
    let output = common::phux_cmd(runner::phux_bin())
        .args([
            "api",
            "--remote",
            "no-enrollment.invalid",
            "call",
            "question.receipt",
            "--request-json",
            &request.to_string(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "question_receipt_unavailable");
    assert_eq!(error["error"]["outcome"], "unknown");
    assert_eq!(error["error"]["operation_id"], "42");
}

#[tokio::test]
async fn api_does_not_send_a_create_to_an_unsupported_peer() {
    use phux_client::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::{caps::ServerFeatureExtSet, wire::frame::FrameKind};

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("old.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(async move {
        ScriptedServer::accept(
            &listener,
            ScriptSpec::new().server_features_ext(ServerFeatureExtSet::default()),
        )
        .await
    });
    let output = tokio::task::spawn_blocking(move || {
        common::phux_cmd(runner::phux_bin())
            .arg("--socket")
            .arg(socket)
            .args([
                "api",
                "call",
                "terminal.create",
                "--request-json",
                &json!({"session":SESSION, "idempotency_key":KEY}).to_string(),
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let error = failure(
        &output,
        1,
        ErrorCode::Unsupported,
        OperationOutcome::NotSent,
    );
    assert_eq!(error.error.idempotency_key.as_deref(), Some(KEY));
    let frames = tokio::time::timeout(std::time::Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(frames.as_slice(), [FrameKind::Hello { .. }]));
}
