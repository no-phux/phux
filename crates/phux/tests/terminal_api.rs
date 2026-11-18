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
    let exact = json!({"execution":created["execution_ref"],"timeout_secs":1});
    let (code, stdout, stderr) = server.run(&[
        "api",
        "call",
        "execution.result",
        "--request-json",
        &exact.to_string(),
    ]);
    assert_eq!(code, 124, "{stderr}");
    let timed_out: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(timed_out["status"], "timed_out");
    assert_eq!(timed_out["observation"]["state"], "running");
    assert!(timed_out["observation"]["outcome"].is_null());
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

#[test]
fn cli_execution_observes_only_its_exact_process() {
    let server = common::ServerGuard::start("exact-execution");
    let inventory: Value = serde_json::from_str(&server.success(&["ls", "--json"])).unwrap();
    let session = inventory["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["name"] == "work")
        .unwrap()["key"]
        .as_str()
        .unwrap();
    let request = json!({"idempotency_key":"12121212121212121212121212121212", "session":session, "argv":["/bin/sh","-c","sleep 0.3; exit 37"]});
    let created: Value = serde_json::from_str(&server.success(&[
        "terminal",
        "create",
        "--request-json",
        &request.to_string(),
    ]))
    .unwrap();
    let mut exact = json!({"execution":created["execution_ref"], "timeout_secs":8});
    let settled: Value = serde_json::from_str(&server.success(&[
        "resource",
        "execution",
        "--request-json",
        &exact.to_string(),
    ]))
    .unwrap();
    assert_eq!(settled["status"], "observed");
    assert_eq!(
        settled["observation"]["outcome"],
        json!({"type":"exited","code":37})
    );
    assert_eq!(settled["execution"], created["execution_ref"]);
    assert!(
        jsonschema::validator_for(&phux_protocol::api::execution_result_schema())
            .unwrap()
            .is_valid(&settled)
    );

    exact["execution"]["instance"] = json!("ffffffffffffffffffffffffffffffff");
    let wrong_instance: Value = serde_json::from_str(&server.success(&[
        "resource",
        "execution",
        "--request-json",
        &exact.to_string(),
    ]))
    .unwrap();
    assert_eq!(wrong_instance["status"], "unavailable");
    assert!(wrong_instance["observation"].is_null());

    exact["execution"]["authority"] = json!("aebagbafaydqqcikbmga2dqpca");
    exact["execution"]["resource"] = json!("aebagbafaydqqcikbmga2dqpca");
    let (code, _, error) = server.run(&[
        "resource",
        "execution",
        "--request-json",
        &exact.to_string(),
    ]);
    assert_eq!(code, 1);
    let error: Value = serde_json::from_str(&error).unwrap();
    assert_eq!(error["error"]["code"], "authority_mismatch");
}

#[test]
fn direct_terminal_run_returns_the_fenced_exit_without_a_script_wrapper() {
    let server = common::ServerGuard::start("direct-terminal-run");
    for (argv, want, status) in [
        (vec!["/usr/bin/true"], 0, "exited"),
        (vec!["/bin/sh", "-c", "exit 47"], 47, "exited"),
        (vec!["/bin/sleep", "30"], 125, "timed_out"),
        (
            vec!["/nonexistent/phux-no-such-executable"],
            126,
            "launch_failed",
        ),
    ] {
        let mut args = vec![
            "terminal",
            "run",
            "--session",
            "work",
            "--timeout",
            "1",
            "--",
        ];
        args.extend(argv);
        let (exit, stdout, stderr) = server.run(&args);
        assert_eq!(exit, want, "stdout={stdout}\nstderr={stderr}");
        let doc: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(doc["status"], status);
        assert_eq!(doc["result"]["execution"], doc["receipt"]["execution_ref"]);
        assert_eq!(doc["session"], doc["receipt"]["session"]);
        assert_eq!(doc["idempotency_key"], doc["receipt"]["idempotency_key"]);
        if status == "exited" {
            assert_eq!(doc["result"]["observation"]["outcome"]["code"], want);
        }
    }
}

#[tokio::test]
async fn direct_terminal_run_recovers_a_lost_launch_only_by_reading() {
    use phux_client::testkit::{OrgFixture, ScriptSpec, ScriptedServer};
    use phux_protocol::{
        caps::{ServerFeatureExt, ServerFeatureExtSet},
        ids::OperationId,
        wire::frame::{Command, CommandResult, FrameKind, SessionLifetime},
    };
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("terminal-run.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let mut fixture = OrgFixture::new();
    let session = fixture.session("lost", SessionLifetime::Persistent);
    let operation = OperationId::new([0x12; 16]).unwrap();
    let features = ServerFeatureExtSet::with(&[
        ServerFeatureExt::Organization,
        ServerFeatureExt::CreatePlacement,
        ServerFeatureExt::ApiDescription,
    ]);
    let first = ScriptSpec::new()
        .organization(fixture.clone())
        .server_features_ext(features)
        .mutation_reply(CommandResult::Accepted { operation });
    let recovery = ScriptSpec::new()
        .organization(fixture)
        .server_features_ext(features);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let original = tokio::spawn(ScriptedServer::on_stream(stream, first).run());
        let recovery = ScriptedServer::accept(&listener, recovery).await;
        (original.await.unwrap(), recovery)
    });
    let output = tokio::task::spawn_blocking(move || {
        common::phux_cmd(runner::phux_bin())
            .arg("--socket")
            .arg(socket)
            .args([
                "terminal",
                "run",
                "--session",
                "lost",
                "--idempotency-key",
                "12121212121212121212121212121212",
                "--",
                "/usr/bin/true",
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let doc: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["status"], "launch_unknown");
    assert_eq!(doc["session"], session.to_string());
    assert_eq!(doc["idempotency_key"], "12121212121212121212121212121212");
    let (original, recovery) = tokio::time::timeout(std::time::Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        original
            .iter()
            .filter(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::ResourceCreate { .. },
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(matches!(recovery.as_slice(), [
        FrameKind::Hello { .. },
        FrameKind::Command { command: Command::GetCreation { operation: key, .. }, .. },
    ] if *key == operation));
}
