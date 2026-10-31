//! Public schema/runtime negatives and real exec/result projection agreement.
use super::*;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

fn session(server: &common::ServerGuard) -> String {
    let inventory: Value = serde_json::from_str(&server.success(&["ls", "--json"])).unwrap();
    inventory["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "work")
        .unwrap()["key"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn create(server: &common::ServerGuard, request: &Value) -> Value {
    serde_json::from_str(&server.success(&[
        "terminal",
        "create",
        "--request-json",
        &request.to_string(),
    ]))
    .unwrap()
}
fn settled(server: &common::ServerGuard, key: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let result: Value = serde_json::from_str(&server.success(&[
            "resource",
            "show",
            "--json",
            &format!("^{key}"),
        ]))
        .unwrap();
        if result["execution"]["state"] == "failed"
            || (result["execution"]["state"] == "completed" && result["terminal_id"].is_null())
        {
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "execution did not settle: {result}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn representable_invalid_requests_fail_schema_runtime_and_real_cli() {
    let server = common::ServerGuard::start("api-schema-negatives");
    let session = session(&server);
    let description: Value =
        serde_json::from_str(&server.success(&["terminal", "describe"])).unwrap();
    let schema = &description["operations"][0];
    let validator = jsonschema::validator_for(&schema["input_schema"]).unwrap();
    let error_validator = jsonschema::validator_for(&schema["error_schema"]).unwrap();
    let base = json!({"session":session, "idempotency_key":"abcdefabcdefabcdefabcdefabcdefab", "argv":["/bin/true"]});
    let mut negatives = vec![
        ("argv", json!(vec!["x"; 257])),
        ("idempotency_key", json!("00000000000000000000000000000000")),
        ("session", json!("aaaaaaaaaaaaaaaaaaaaaaaaaa")),
        ("cwd", json!("relative")),
        ("env", json!({"PHUX_SOCKET":"private"})),
        ("env", json!({"TERM":"private"})),
        (
            "placement",
            json!({"type":"beside", "entry":"x", "resource":"x", "instance":"x", "activation":0, "axis":"horizontal", "ratio":2}),
        ),
    ];
    let beside = json!({"type":"beside", "entry":"11111111111111111111111111111111", "resource":base["session"], "instance":"11111111111111111111111111111111", "activation":1, "axis":"horizontal", "ratio":0.5});
    for (field, value) in [
        ("entry", json!("bad")),
        ("resource", json!("bad")),
        ("instance", json!("bad")),
        ("activation", json!(0)),
        ("ratio", json!(2)),
        ("ratio", json!(0)),
        ("ratio", json!(1)),
    ] {
        let mut placement = beside.clone();
        placement[field] = value;
        negatives.push(("placement", placement));
    }
    for (field, value) in negatives {
        let mut request = base.clone();
        request[field] = value;
        assert!(!validator.is_valid(&request), "schema accepted {request}");
        let dto: phux_protocol::api::TerminalCreate =
            serde_json::from_value(request.clone()).unwrap();
        assert!(dto.command().is_err());
        let (code, _, error) =
            server.run(&["terminal", "create", "--request-json", &request.to_string()]);
        assert_ne!(code, 0);
        let error: Value = serde_json::from_str(&error).unwrap();
        assert!(
            error_validator.is_valid(&error),
            "invalid error DTO: {error}"
        );
        assert_eq!(error["error"]["outcome"], "not_sent");
    }
    assert!(validator.is_valid(&base));
}

#[test]
fn retained_durable_inspection_agrees_with_receipts_for_real_execs() {
    let server = common::ServerGuard::start("api-exec-evidence");
    let session = session(&server);
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad");
    std::fs::write(&bad, "#!/no/such/interpreter\n").unwrap();
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o700)).unwrap();
    for (n, argv, state, code) in [
        (
            1,
            vec![dir.path().join("missing").to_string_lossy().into_owned()],
            "failed",
            None,
        ),
        (2, vec![bad.to_string_lossy().into_owned()], "failed", None),
        (
            3,
            vec!["/bin/sh".into(), "-c".into(), "exit 127".into()],
            "completed",
            Some(127),
        ),
        (
            4,
            vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
            "completed",
            Some(0),
        ),
        (
            5,
            vec!["/bin/sh".into(), "-c".into(), "exit 7".into()],
            "completed",
            Some(7),
        ),
    ] {
        let retry = format!("{n:032x}");
        let created = create(
            &server,
            &json!({"session":session, "idempotency_key":retry, "argv":argv}),
        );
        let resource = created["resource"].as_str().unwrap();
        let record = settled(&server, resource);
        assert_eq!(record["schema_version"], 2);
        assert_eq!(record["execution"]["state"], state);
        if let Some(code) = code {
            assert_eq!(
                record["execution"]["outcome"],
                json!({"type":"exited", "code":code})
            );
            assert!(record["execution"]["diagnostic"].is_null());
        } else {
            assert_eq!(record["execution"]["outcome"]["type"], "unobserved");
            assert_eq!(record["execution"]["diagnostic"]["stage"], "exec");
        }
        let receipt: Value = serde_json::from_str(&server.success(&[
            "terminal",
            "operation",
            "--session",
            &session,
            "--idempotency-key",
            &retry,
        ]))
        .unwrap();
        assert_eq!(record["execution"], receipt["execution"]);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let inventory = runtime
            .block_on(phux_client::org::fetch_from_socket(&server.socket))
            .unwrap()
            .unwrap();
        let row = inventory
            .resources
            .iter()
            .find(|r| r.key.to_string() == resource)
            .unwrap();
        assert_eq!(
            row.activation.unwrap().diagnostic,
            serde_json::from_value(record["execution"]["diagnostic"].clone()).unwrap()
        );
        let schema = phux_protocol::api::resource_observed_schema();
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.is_valid(&record));
        let mut invalid = record.clone();
        invalid["execution"]["activation"] = json!(0);
        assert!(!validator.is_valid(&invalid));
        invalid = record.clone();
        invalid["resource"] = json!("bad");
        assert!(!validator.is_valid(&invalid));
    }
}
