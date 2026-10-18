//! Upload consumers exercised as real CLI and MCP processes on disposable
//! servers. The `#[ignore]`d cases run in `just e2e`.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod common;
#[path = "common/listeners.rs"]
mod listeners;
#[path = "common/runner.rs"]
mod runner;

use common::ServerGuard;
use serde_json::Value;

/// The `phux-mcp` binary: `$PHUX_TEST_MCP_BIN`, else the one a workspace
/// build put beside `phux`.
fn mcp_bin() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("PHUX_TEST_MCP_BIN") {
        return path.into();
    }
    let built = std::path::Path::new(runner::phux_bin()).with_file_name("phux-mcp");
    assert!(
        built.exists(),
        "{} is missing: build the workspace (`cargo nextest run --workspace`) or set PHUX_TEST_MCP_BIN",
        built.display()
    );
    built
}

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
    assert!(remote_help.contains("except `upload`, `download`, and calls with `--json`: these never pair and refuse unregistered hosts"), "{remote_help}");
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
#[ignore = "spawns a real private server; runs in the e2e lane"]
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
#[ignore = "spawns real private servers; runs in the e2e lane"]
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
#[ignore = "real MCP tools/call on a private server; runs in the e2e lane"]
fn mcp_upload_calls_keep_completed_refused_and_unknown_receipts() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    let mcp = mcp_bin();
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

#[test]
#[ignore = "two private servers and real MCP; runs in the e2e lane"]
fn cli_and_mcp_upload_to_satellite_retain_only_on_owner_without_terminal_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let leaf_root = dir.path().join("leaf-uploads");
    let hub_root = dir.path().join("hub-uploads");
    let input_log = dir.path().join("terminal-input");
    let ready = dir.path().join("recorder-ready");
    let baseline = b"upload-calibration-no-newline";
    // The hub reaches the leaf over `ssh://`: a fake ssh runs the leaf's
    // `phux stdio-bridge`, the owner-socket route. A network satellite would
    // need `paired` and a hub certificate (ADR-0179); the upload path is the
    // same either way.
    let leaf = ServerGuard::builder("upload-leaf")
        .env_clear()
        .env("PHUX_UPLOAD_DIR", &leaf_root)
        .seed_command(format!(
            "/bin/sh -c '/bin/stty -icanon -echo min 1 time 0 && : > \"{}\" && printf ready > \"{}\" && exec /bin/cat >> \"{}\"'",
            input_log.display(), ready.display(), input_log.display()
        ))
        .start();
    let fake_ssh = fake_ssh_to(dir.path(), &leaf.socket);
    let config = dir.path().join("hub-config");
    std::fs::create_dir_all(config.join("phux")).unwrap();
    std::fs::write(
        config.join("phux/config.toml"),
        "[[satellites]]\nname = \"leaf\"\nendpoint = \"ssh://leaf\"\n",
    )
    .unwrap();
    let hub = ServerGuard::builder("upload-hub")
        .env_clear()
        .env("XDG_CONFIG_HOME", &config)
        .env("PHUX_SSH", &fake_ssh)
        .env("PHUX_UPLOAD_DIR", &hub_root)
        .seed_command("/bin/cat")
        .start_with(|cmd| {
            cmd.arg("--hub");
        });
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    wait_for_recorder_bytes(&ready, b"ready");
    inject_fixture_input(&rt, &leaf, baseline);
    wait_for_recorder_bytes(&input_log, baseline);
    // Calibration is intentional fixture input, so take upload snapshots only after it drains.
    let leaf_before = rt
        .block_on(phux_client::state::get_state(&leaf.socket))
        .unwrap();
    let owner = phux_protocol::ResourceId::satellite(
        "leaf",
        leaf_before.snapshot().focused_resource.local_id().unwrap(),
    );
    // Wait for inventory, never retry an upload: a missing link is a fixture
    // readiness condition, while a transfer with uncertain completion is not.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let hub_before = loop {
        let view = rt
            .block_on(phux_client::state::get_state(&hub.socket))
            .unwrap();
        if view.snapshot().resources.iter().any(|row| row.id == owner) {
            break view;
        }
        assert!(std::time::Instant::now() < deadline, "leaf never linked");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let source = dir.path().join("source.bin");
    let data = vec![0x71; phux_protocol::wire::frame::MAX_FILE_UPLOAD_CHUNK + 31];
    std::fs::write(&source, &data).unwrap();
    let target = owner.to_string();
    let cli: Value = serde_json::from_str(&hub.success(&[
        "upload",
        &target,
        source.to_str().unwrap(),
        "--json",
    ]))
    .unwrap();
    assert_owner_upload(&cli, &owner, &leaf_root, &data);
    let mcp = satellite_mcp_upload(&hub, &target, &source);
    assert_owner_upload(&mcp, &owner, &leaf_root, &data);
    assert_two_retained_artifacts(&leaf_root, &data);
    assert!(
        !hub_root.exists() || std::fs::read_dir(&hub_root).unwrap().next().is_none(),
        "gateway must not retain a duplicate artifact"
    );
    for (server, before) in [(&leaf, leaf_before), (&hub, hub_before)] {
        let after = rt
            .block_on(phux_client::state::get_state(&server.socket))
            .unwrap();
        assert_eq!(
            before.snapshot(),
            after.snapshot(),
            "upload changed attached clients, grid, input lease or resources"
        );
    }
    assert_eq!(std::fs::read(&input_log).unwrap(), baseline);
    // A known newline-free suffix must drain too: don't accept a stalled recorder
    // as no-input proof. Every recorded byte must be deliberate fixture input.
    let barrier = b"upload-drain-no-newline";
    inject_fixture_input(&rt, &leaf, barrier);
    wait_for_recorder_bytes(
        &input_log,
        &[baseline.as_slice(), barrier.as_slice()].concat(),
    );
    assert_upload_servers_cleaned_up(hub, leaf);
}

fn assert_upload_servers_cleaned_up(hub: ServerGuard, leaf: ServerGuard) {
    let pids = [hub.pid(), leaf.pid()];
    let sockets = [hub.socket.clone(), leaf.socket.clone()];
    drop(hub);
    drop(leaf);
    for pid in pids {
        assert!(!common::process_exists(pid), "test server {pid} leaked");
    }
    for socket in sockets {
        assert!(!socket.exists(), "test socket leaked: {}", socket.display());
    }
}

fn wait_for_recorder_bytes(path: &std::path::Path, expected: &[u8]) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("read recorder {}: {error}", path.display()),
        };
        assert!(
            expected.starts_with(&bytes),
            "unexpected recorder bytes: {bytes:?}"
        );
        if bytes == expected {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "recorder did not drain {expected:?}: {bytes:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn inject_fixture_input(rt: &tokio::runtime::Runtime, leaf: &ServerGuard, bytes: &[u8]) {
    rt.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let pane = phux_client::state::get_state(&leaf.socket)
                .await
                .unwrap()
                .snapshot()
                .focused_resource
                .clone();
            assert!(pane.local_id().is_some(), "calibrate only the owned leaf");
            phux_client::send_keys::paste_to(
                &leaf.socket,
                pane,
                bytes.to_vec(),
                phux_protocol::input::paste::PasteTrust::Trusted,
            )
            .await
            .unwrap();
        })
        .await
        .expect("fixture input timed out");
    });
}

fn assert_two_retained_artifacts(root: &std::path::Path, data: &[u8]) {
    let artifacts: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            if path
                .file_name()
                .unwrap()
                .as_encoded_bytes()
                .starts_with(b".")
            {
                assert_eq!(
                    path.extension().unwrap(),
                    "meta",
                    "no partial upload remains"
                );
                false
            } else {
                true
            }
        })
        .collect();
    assert_eq!(artifacts.len(), 2, "one retained artifact per invocation");
    for path in artifacts {
        assert_eq!(std::fs::read(path).unwrap(), data);
    }
}

fn assert_owner_upload(
    document: &Value,
    owner: &phux_protocol::ResourceId,
    root: &std::path::Path,
    data: &[u8],
) {
    assert_eq!(document["receipt"]["outcome"], "completed");
    assert_eq!(
        document["receipt"]["terminal_id"],
        serde_json::to_value(owner).unwrap()
    );
    let path = std::path::Path::new(document["receipt"]["path"].as_str().unwrap());
    assert!(path.starts_with(root), "receipt must name the owner's path");
    assert_eq!(std::fs::read(path).unwrap(), data);
}

fn satellite_mcp_upload(server: &ServerGuard, target: &str, source: &std::path::Path) -> Value {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    let mcp = mcp_bin();
    let mut cmd = common::phux_cmd(mcp);
    server.xdg.apply(&mut cmd);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "{}", serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{"name":"phux_upload", "arguments":{"target":target, "path":source, "socket":server.socket, "timeout_secs":30}}})).unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    assert!(
        reader.read_line(&mut line).unwrap() > 0,
        "MCP ended before reply"
    );
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reply: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

/// A fake `ssh` that runs `phux stdio-bridge` on `socket`, whatever host it
/// is asked for: the hub's `ssh://` link to a local leaf.
fn fake_ssh_to(dir: &std::path::Path, socket: &std::path::Path) -> std::path::PathBuf {
    let fake_ssh = dir.join("fake-ssh");
    std::fs::write(
        &fake_ssh,
        format!(
            "#!/bin/sh\nexec \"{}\" stdio-bridge --socket \"{}\"\n",
            runner::phux_bin(),
            socket.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&fake_ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    fake_ssh
}
