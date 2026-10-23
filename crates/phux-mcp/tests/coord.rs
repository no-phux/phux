//! Mail and claims over real stdio MCP, against a keeper this test owns on
//! a disposable store: the tools call the store in-process.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const PROFILE: &str = "mcp-coord-test";

/// Run `calls` as `tools/call` requests in one adapter speaking as `@749`.
fn calls(root: &std::path::Path, calls: &[(&str, Value)]) -> Vec<Value> {
    let program = ["CARGO_BIN_EXE_phux-mcp", "NEXTEST_BIN_EXE_phux-mcp"]
        .into_iter()
        .find_map(std::env::var_os)
        .unwrap();
    let mut child = Command::new(program)
        .env_clear()
        .env("HOME", root)
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("PHUX_PROFILE", PROFILE)
        .env("PHUX_TERMINAL_ID", "749")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut exchange = |request: Value| {
        writeln!(input, "{request}").unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    exchange(
        json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
    );
    let replies = (1..)
        .zip(calls)
        .map(|(id, (name, arguments))| {
            let reply = exchange(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}}));
            assert_eq!(reply["id"], id);
            reply["result"]["structuredContent"].clone()
        })
        .collect();
    drop(input);
    assert!(child.wait().unwrap().success());
    replies
}

#[test]
fn mail_unknown_reply_keeps_durable_identity_across_cached_failure_and_adapter_restart() {
    use std::os::unix::net::UnixListener;
    let root = tempfile::Builder::new()
        .prefix("phux-mcp-retry-")
        .tempdir_in("/tmp")
        .unwrap();
    let expected = phux_work::Endpoint::new(
        &root.path().join(format!("state/phux-{PROFILE}")),
        &root.path().join(format!("run/phux-{PROFILE}")),
    );
    let actual = phux_work::Endpoint {
        store: expected.store.clone(),
        socket: root.path().join("actual.sock"),
    };
    let serving = actual.clone();
    let keeper =
        std::thread::spawn(move || phux_work::run_keeper(&serving, Duration::from_secs(5)));
    let deadline = Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(&actual.socket).is_err() {
        assert!(Instant::now() < deadline, "keeper never bound");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::fs::create_dir_all(expected.socket.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&expected.socket).unwrap();
    let proxy = std::thread::spawn(move || {
        for index in 0..3 {
            let (mut caller, _) = listener.accept().unwrap();
            caller
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut line = String::new();
            BufReader::new(&caller).read_line(&mut line).unwrap();
            let mut backend = UnixStream::connect(&actual.socket).unwrap();
            backend
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            backend.write_all(line.as_bytes()).unwrap();
            let mut reply = String::new();
            BufReader::new(&backend).read_line(&mut reply).unwrap();
            assert!(!reply.is_empty());
            if index != 0 {
                caller.write_all(reply.as_bytes()).unwrap();
            }
            // The first committed reply is deliberately discarded.
        }
    });
    let key = "12345678123456781234567812345678";
    let first_alias = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let second_alias = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let send = |alias| json!({"to":["@730"], "thread":"retry", "message":"once", "operation_id":key, "idempotency_key":alias});
    let failed = calls(
        root.path(),
        &[
            ("phux_mail_send", send(first_alias)),
            ("phux_mail_send", send(first_alias)),
            (
                "phux_operation_status",
                json!({"idempotency_key":first_alias}),
            ),
        ],
    );
    assert_eq!(failed[0]["error"]["recovery"]["operation_id"], key);
    assert_eq!(
        failed[0]["error"]["recovery"]["scope"]["kind"],
        "work_store"
    );
    assert_eq!(
        failed[0]["error"]["recovery"]["effect_certainty"],
        "unknown"
    );
    assert_eq!(
        failed[0], failed[1],
        "same adapter alias returns the cached failure"
    );
    assert_eq!(failed[2]["adapter_receipt_id"], first_alias);
    assert_eq!(failed[2]["scope"]["kind"], "mcp_adapter");
    assert_eq!(failed[2]["state"], "failed");
    let recovered = calls(
        root.path(),
        &[
            ("phux_mail_send", send(second_alias)),
            (
                "phux_mail_read",
                json!({"as":"@730", "all":true, "peek":true}),
            ),
        ],
    );
    assert_eq!(recovered[0]["operation_id"], key);
    assert_eq!(recovered[0]["replayed"], true);
    assert_eq!(recovered[1]["mail"].as_array().unwrap().len(), 1);
    proxy.join().unwrap();
    keeper.join().unwrap().unwrap();
}

#[test]
fn mail_and_claims_reach_the_store_without_the_cli() {
    let root = tempfile::Builder::new()
        .prefix("phux-mcp-coord-")
        .tempdir_in("/tmp")
        .unwrap();
    let endpoint = phux_work::Endpoint::new(
        &root.path().join(format!("state/phux-{PROFILE}")),
        &root.path().join(format!("run/phux-{PROFILE}")),
    );
    let keeper = {
        let endpoint = endpoint.clone();
        std::thread::spawn(move || phux_work::run_keeper(&endpoint, Duration::from_secs(2)))
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(&endpoint.socket).is_err() {
        assert!(Instant::now() < deadline, "keeper never bound");
        std::thread::sleep(Duration::from_millis(5));
    }

    let replies = calls(
        root.path(),
        &[
            (
                "phux_mail_send",
                json!({"to": ["opencode@730"], "thread": "pr-1", "message": "rebased"}),
            ),
            ("phux_mail_read", json!({"as": "@730"})),
            (
                "phux_mail_ack",
                json!({"id": "0123456789abcdef0123456789abcdef", "as": "@730"}),
            ),
            ("phux_claim_take", json!({"paths": ["/work/./a/../b"]})),
            ("phux_claim_list", json!({"holder": "@749"})),
        ],
    );
    let [sent, read, missing, taken, listed] = replies.as_slice() else {
        panic!("five replies: {replies:?}");
    };
    assert_eq!(sent["schema"], "phux.coord/v1");
    assert_eq!(
        sent["mail"]["from"], "@749",
        "the adapter speaks as its pane"
    );
    assert!(sent.get("bells").is_none());
    assert_eq!(read["reader"], "@730");
    assert_eq!(read["mail"][0]["body"], "rebased");
    assert_eq!(missing["error"]["code"], "mail_not_found");
    assert_eq!(missing["exit_code"], 1);
    assert_eq!(taken["claim"]["outcome"], "granted");
    assert_eq!(taken["claim"]["claims"][0]["path"], "/work/b");
    assert_eq!(listed["claims"][0]["holder"], "@749");

    keeper.join().unwrap().unwrap();
}
