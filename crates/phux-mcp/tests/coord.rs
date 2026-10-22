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
