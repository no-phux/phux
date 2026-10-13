//! Upload consumers exercised as real CLI processes on disposable servers.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod common;
#[path = "common/runner.rs"]
mod runner;

use common::ServerGuard;
use serde_json::Value;

#[test]
fn upload_help_and_malformed_arguments_do_not_connect_or_read() {
    let help = common::phux_cmd(runner::phux_bin())
        .args(["upload", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);
    for expected in [
        "--extension",
        "--timeout",
        "--json",
        "--remote",
        "filesystem",
        "do not blindly retry",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    // The flattened --remote option must not contradict upload's headless policy.
    let remote_help = text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(remote_help.contains("except `upload` and calls with `--json`: these never pair and refuse unregistered hosts"), "{remote_help}");
    for options in [
        vec!["--timeout", "0"],
        vec!["--timeout", "3601"],
        vec!["--timeout", "no"],
        vec!["--extension", "../bad"],
        vec!["--extension", ""],
        vec!["--extension", "abcdefghijklmnopq"],
        vec!["--remote", "mini"],
        vec!["--bogus"],
    ] {
        let out = common::phux_cmd(runner::phux_bin())
            .args([
                "--socket",
                "/nonexistent/upload-test.sock",
                "upload",
                "@1",
                "/nonexistent/upload-source",
                "--json",
            ])
            .args(options)
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !err.contains("could not read upload file"),
            "read before validation: {err}"
        );
        assert!(
            !err.contains("no server"),
            "connected before validation: {err}"
        );
    }
    for args in [
        vec!["upload"],
        vec!["upload", "@1"],
        vec!["upload", "@1", "a", "b"],
    ] {
        assert!(
            !common::phux_cmd(runner::phux_bin())
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
}

#[test]
#[ignore = "spawns a real private server; run explicitly with two test threads"]
fn upload_retains_multichunk_and_empty_files_without_attaching() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("uploads");
    let server = ServerGuard::builder("upload-retain")
        .env("PHUX_UPLOAD_DIR", &root)
        .start();
    let source = dir.path().join("local.bin");
    let data = vec![0x5a; phux_protocol::wire::frame::MAX_FILE_UPLOAD_CHUNK + 123];
    std::fs::write(&source, &data).unwrap();
    let before: Value = serde_json::from_str(&server.success(&["ls", "--json"])).unwrap();
    let output = server.success(&[
        "upload",
        "work",
        source.to_str().unwrap(),
        "--extension",
        "mp4",
        "--json",
    ]);
    let doc: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(doc["receipt"]["outcome"], "completed");
    let path = std::path::Path::new(doc["receipt"]["path"].as_str().unwrap());
    assert!(path.starts_with(&root));
    assert_eq!(path.extension().unwrap(), "mp4");
    assert_eq!(std::fs::read(path).unwrap(), data);
    std::fs::write(&source, []).unwrap();
    let path = server.success(&["upload", "work", source.to_str().unwrap()]);
    assert_eq!(std::fs::read(path.trim()).unwrap(), Vec::<u8>::new());
    let huge = dir.path().join("huge.bin");
    std::fs::File::create(&huge)
        .unwrap()
        .set_len(phux_protocol::wire::frame::MAX_FILE_UPLOAD_SIZE + 1)
        .unwrap();
    let (status, output, error) = server.run(&["upload", "work", huge.to_str().unwrap(), "--json"]);
    assert_eq!(status, 1);
    assert!(output.is_empty());
    assert!(error.contains("64 MiB upload limit"));
    let after: Value = serde_json::from_str(&server.success(&["ls", "--json"])).unwrap();
    assert_eq!(
        before, after,
        "upload changed session/attached-client state"
    );
}

#[test]
#[ignore = "spawns real private servers; run explicitly with two test threads"]
fn upload_refused_and_unknown_keep_receipts_and_do_not_retry() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("local.bin");
    std::fs::write(&source, b"bytes").unwrap();
    let root = dir.path().join("refused");
    std::fs::write(&root, b"not a directory").unwrap();
    let server = ServerGuard::builder("upload-refuse")
        .env("PHUX_UPLOAD_DIR", &root)
        .start();
    let (status, output, warning) =
        server.run(&["upload", "work", source.to_str().unwrap(), "--json"]);
    assert_eq!(status, 1, "{warning}");
    let doc: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(doc["receipt"]["outcome"], "refused");
    assert!(doc["receipt"]["path"].is_null());
    assert!(warning.contains("do not blindly retry"));
    let hold = dir.path().join("hold");
    let root = dir.path().join("unknown");
    let server = ServerGuard::builder("upload-unknown")
        .env("PHUX_UPLOAD_DIR", &root)
        .env("PHUX_TEST_UPLOAD_HOLD", &hold)
        .start();
    let started = std::time::Instant::now();
    let (status, output, warning) = server.run(&[
        "upload",
        "work",
        source.to_str().unwrap(),
        "--timeout",
        "1",
        "--json",
    ]);
    std::fs::write(hold.join("release"), []).unwrap();
    assert_eq!(status, 1, "{warning}");
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    let doc: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(doc["receipt"]["outcome"], "unknown");
    assert!(doc["receipt"]["path"].is_null());
    assert!(warning.contains("do not blindly retry"));
}

#[test]
#[ignore = "real MCP tools/call; build phux-mcp and set PHUX_TEST_MCP_BIN to its checkout binary"]
fn mcp_upload_calls_keep_completed_refused_and_unknown_receipts() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    let mcp = std::env::var("PHUX_TEST_MCP_BIN").expect("explicit checkout MCP binary");
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("local.bin");
    let data = vec![0x33; phux_protocol::wire::frame::MAX_FILE_UPLOAD_CHUNK + 11];
    std::fs::write(&source, &data).unwrap();
    for outcome in ["completed", "refused", "unknown"] {
        let root = dir.path().join(outcome);
        let hold = dir.path().join("hold");
        let mut builder =
            ServerGuard::builder(&format!("mcp-upload-{outcome}")).env("PHUX_UPLOAD_DIR", &root);
        if outcome == "refused" {
            std::fs::write(&root, b"not a directory").unwrap();
        }
        if outcome == "unknown" {
            builder = builder.env("PHUX_TEST_UPLOAD_HOLD", &hold);
        }
        let server = builder.start();
        let mut cmd = common::phux_cmd(&mcp);
        server.xdg.apply(&mut cmd);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        writeln!(
            input,
            "{}",
            serde_json::json!({"jsonrpc":"2.0", "id":1,"method":"tools/list"})
        )
        .unwrap();
        writeln!(input, "{}", serde_json::json!({"jsonrpc":"2.0", "id":2,"method":"tools/call", "params":{"name":"phux_upload", "arguments":{"target":"work", "path": source, "socket":server.socket, "timeout_secs": if outcome == "unknown" {1} else {60}}}})).unwrap();
        // Keep stdin alive until tools/call completes: EOF intentionally cancels in-flight MCP calls.
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut replies = Vec::new();
        for _ in 0..2 {
            let mut line = String::new();
            assert!(
                reader.read_line(&mut line).unwrap() > 0,
                "MCP ended before reply"
            );
            replies.push(serde_json::from_str::<Value>(&line).unwrap());
        }
        drop(input);
        let output = child.wait_with_output().unwrap();
        if outcome == "unknown" {
            std::fs::write(hold.join("release"), []).unwrap();
        }
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let catalog = &replies.iter().find(|reply| reply["id"] == 1).unwrap()["result"]["tools"];
        let tool = catalog
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "phux_upload")
            .unwrap();
        assert_eq!(tool["annotations"]["readOnlyHint"], false);
        assert_eq!(tool["annotations"]["destructiveHint"], true);
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        let result = &replies.iter().find(|reply| reply["id"] == 2).unwrap()["result"];
        assert_eq!(result["isError"], outcome != "completed");
        let document: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(document["receipt"]["outcome"], outcome);
        if outcome == "completed" {
            let path = std::path::Path::new(document["receipt"]["path"].as_str().unwrap());
            assert!(path.starts_with(&root));
            assert_eq!(std::fs::read(path).unwrap(), data);
        } else {
            assert!(document["receipt"]["path"].is_null());
            assert!(
                document["warning"]
                    .as_str()
                    .unwrap()
                    .contains("do not blindly retry")
            );
        }
    }
}
