//! The public terminal workflow as a CLI, not an in-process handler test.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
#[path = "common/runner.rs"]
mod runner;
use serde_json::{Value, json};
use std::{io::Write, process::Stdio};

#[path = "terminal_api/hardening.rs"]
mod hardening;

#[test]
fn api_lookup_dial_failure_preserves_unknown_outcome() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("missing.sock");
    let key = "abababababababababababababababab";
    let request = json!({"session":"0123456789ABCDEFGHJKMNPQRS", "idempotency_key":key});
    let out = common::phux_cmd(runner::phux_bin())
        .args(["api", "--socket"])
        .arg(socket)
        .args([
            "call",
            "terminal.operation",
            "--request-json",
            &request.to_string(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let error: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(error["error"]["outcome"], "unknown");
    assert_eq!(error["error"]["idempotency_key"], key);
}

#[test]
fn cli_describes_creates_and_recovers_without_caller_rollback() {
    let server = common::ServerGuard::start("terminal-api");
    let description: Value = serde_json::from_str(&server.success(&["api", "list"])).unwrap();
    assert_eq!(description["source"], "connected_server");
    let selected: Value =
        serde_json::from_str(&server.success(&["api", "describe", "terminal.create"])).unwrap();
    assert_eq!(selected["authority"], description["authority"]);
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
    let schema = &selected["operation"];
    assert_eq!(schema, &description["operations"][0]);
    assert!(
        jsonschema::validator_for(&schema["input_schema"])
            .unwrap()
            .is_valid(&request)
    );
    let mut child = server
        .cmd(&["api", "call", "terminal.create", "--request-json", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let created: Value = serde_json::from_slice(&output.stdout).unwrap();
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
    let inspected: Value = serde_json::from_str(&server.success(&[
        "api",
        "call",
        "resource.show",
        "--request-json",
        &created["resource"].to_string(),
    ]))
    .unwrap();
    assert_eq!(inspected["authority"], created["authority"]);
    assert_eq!(inspected["execution"], created["execution"]);
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
