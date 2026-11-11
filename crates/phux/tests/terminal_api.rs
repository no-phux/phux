//! The public terminal workflow as a CLI, not an in-process handler test.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
#[path = "common/runner.rs"]
mod runner;
use serde_json::{Value, json};

#[path = "terminal_api/hardening.rs"]
mod hardening;

#[test]
fn cli_describes_creates_and_recovers_without_caller_rollback() {
    let server = common::ServerGuard::start("terminal-api");
    let description: Value =
        serde_json::from_str(&server.success(&["terminal", "describe"])).unwrap();
    assert_eq!(description["source"], "connected_server");
    let inventory: Value = serde_json::from_str(&server.success(&["ls", "--json"])).unwrap();
    let session = inventory["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["name"] == "work")
        .unwrap()["key"]
        .as_str()
        .unwrap();
    let request = json!({"idempotency_key":"abababababababababababababababab", "session":session, "argv":["/bin/sh","-c","printf 'CLI_API_READY\\n'; exec /bin/sleep 30"]});
    let schema = &description["operations"][0];
    assert!(
        jsonschema::validator_for(&schema["input_schema"])
            .unwrap()
            .is_valid(&request)
    );
    let created: Value = serde_json::from_str(&server.success(&[
        "terminal",
        "create",
        "--request-json",
        &request.to_string(),
    ]))
    .unwrap();
    assert!(
        jsonschema::validator_for(&schema["output_schema"])
            .unwrap()
            .is_valid(&created)
    );
    assert_eq!(created["execution"]["state"], "running");
    let observed: Value = serde_json::from_str(&server.success(&[
        "terminal",
        "operation",
        "--session",
        session,
        "--idempotency-key",
        "abababababababababababababababab",
    ]))
    .unwrap();
    assert_eq!(observed["resource"], created["resource"]);
    assert_eq!(observed["placement"], created["placement"]);
    let (code, _, error) = server.run(&[
        "terminal",
        "operation",
        "--session",
        session,
        "--idempotency-key",
        "ffffffffffffffffffffffffffffffff",
    ]);
    assert_eq!(code, 1);
    let error: Value = serde_json::from_str(&error).unwrap();
    assert_eq!(error["error"]["code"], "receipt_not_retained");
    assert_eq!(error["error"]["outcome"], "unknown");
}
