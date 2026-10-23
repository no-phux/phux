//! Real CLI lease receipts and mail delivery, using an owned keeper and
//! disposable state only.
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]
#[path = "common/ambient.rs"]
mod common;
#[path = "common/runner.rs"]
mod runner;

use serde_json::Value;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct CliStore {
    root: tempfile::TempDir,
    keeper: Child,
}

fn command(root: &Path, pane: &str) -> Command {
    let mut cmd = common::phux_cmd(runner::phux_bin());
    cmd.env("HOME", root)
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("PHUX_PROFILE", "lease-test")
        .env("PHUX_TERMINAL_ID", pane)
        .env("USER", "same-user");
    cmd
}

impl CliStore {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("phux-claims-")
            .tempdir_in("/tmp")
            .unwrap();
        let keeper = command(root.path(), "@749")
            .args(["mission", "keeper", "--idle-exit", "60"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        // Own the Child before any readiness check can fail or panic.
        let mut fixture = Self { root, keeper };
        let endpoint = phux_work::Endpoint::new(
            &fixture.root.path().join("state/phux-lease-test"),
            &fixture.root.path().join("run/phux-lease-test"),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::os::unix::net::UnixStream::connect(&endpoint.socket).is_err() {
            assert!(
                fixture.keeper.try_wait().unwrap().is_none(),
                "owned keeper exited before binding"
            );
            assert!(
                Instant::now() < deadline,
                "owned keeper never bound its isolated socket"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        fixture
    }

    fn run(&self, pane: &str, args: &[&str]) -> Output {
        command(self.root.path(), pane).args(args).output().unwrap()
    }
    fn json(&self, pane: &str, args: &[&str], code: i32) -> Value {
        let out = self.run(pane, args);
        assert_eq!(
            out.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = if out.stdout.is_empty() {
            &out.stderr
        } else {
            &out.stdout
        };
        serde_json::from_slice(bytes).unwrap_or_else(|err| {
            panic!(
                "invalid CLI JSON: {err}; stdout={}; stderr={}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }
}
impl Drop for CliStore {
    fn drop(&mut self) {
        // This is our exact spawned Child, never an inferred PID or an installed server.
        let _ = self.keeper.kill();
        let _ = self.keeper.wait();
    }
}

/// A real pre-fix wire shape: typed payload decoding happens before its bare
/// package-version check. These requests must never reach legacy dispatch.
#[derive(serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[allow(
    dead_code,
    reason = "validate legacy wire fields without executing legacy mutations"
)]
enum LegacyRequest {
    List(phux_work::ListFilter),
    TakeClaim(phux_work::TakeClaim),
    ReleaseClaim { holder: String, paths: Vec<String> },
}

struct LegacyCli {
    root: tempfile::TempDir,
    endpoint: phux_work::Endpoint,
    legacy: Option<std::thread::JoinHandle<Vec<String>>>,
    fence: u64,
}

impl LegacyCli {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("phux-legacy-")
            .tempdir_in("/tmp")
            .unwrap();
        let endpoint = phux_work::Endpoint::new(
            &root.path().join("state/phux-lease-test"),
            &root.path().join("run/phux-lease-test"),
        );
        let mut store = phux_work::Store::open(&endpoint.store).unwrap();
        let phux_work::ClaimOutcome::Granted(held) = store
            .take_claim(&phux_work::TakeClaim {
                holder: "@749".into(),
                paths: vec!["/workspace/a".into()],
                exclusive: true,
                ttl_secs: 600,
                note: "persist across keeper upgrade".into(),
            })
            .unwrap()
        else {
            panic!("initial lease")
        };
        let fence = held[0].fence;
        drop(store);
        std::fs::create_dir_all(endpoint.socket.parent().unwrap()).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&endpoint.socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let socket = endpoint.socket.clone();
        let legacy = Some(std::thread::spawn(move || serve_legacy(&listener, &socket)));
        Self {
            root,
            endpoint,
            legacy,
            fence,
        }
    }
    fn json(&self, args: &[&str], code: i32) -> Value {
        let out = command(self.root.path(), "@749")
            .env("PHUX_WORK_IDLE_EXIT_SECS", "1")
            .args(args)
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = if out.stdout.is_empty() {
            &out.stderr
        } else {
            &out.stdout
        };
        serde_json::from_slice(bytes).unwrap()
    }
}

impl Drop for LegacyCli {
    fn drop(&mut self) {
        retire_private_keeper(&self.endpoint);
        if let Some(legacy) = self.legacy.take() {
            let _ = legacy.join();
        }
    }
}

fn serve_legacy(listener: &std::os::unix::net::UnixListener, socket: &Path) -> Vec<String> {
    use std::io::{BufRead as _, Write as _};
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        let Ok((mut stream, _)) = listener.accept() else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(&stream)
            .read_line(&mut line)
            .unwrap();
        let envelope: Value = serde_json::from_str(&line).unwrap();
        seen.push(envelope["request"]["op"].as_str().unwrap().into());
        if serde_json::from_value::<LegacyRequest>(envelope["request"].clone()).is_err() {
            writeln!(stream, "{}", serde_json::json!({"result": "fault", "value": {"kind": "bad_request", "message": "legacy typed parser rejected operation"}})).unwrap();
            continue;
        }
        assert_ne!(
            envelope["build"],
            env!("CARGO_PKG_VERSION"),
            "same-version keeper must not retain legacy mutation semantics"
        );
        // Remove only the listener we own, before replying; never unlink a
        // replacement keeper's socket after it starts.
        std::fs::remove_file(socket).unwrap();
        writeln!(stream, "{}", serde_json::json!({"result": "fault", "value": {"kind": "stale", "keeper": env!("CARGO_PKG_VERSION")}})).unwrap();
        return seen;
    }
    let _ = std::fs::remove_file(socket);
    seen
}

fn retire_private_keeper(endpoint: &phux_work::Endpoint) -> bool {
    use std::io::{BufRead as _, Write as _};
    if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&endpoint.socket) {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
        let _ = writeln!(
            stream,
            "{}",
            serde_json::json!({"build": "isolated-test-finished", "request": {"op": "list", "repo": null, "archived": false}})
        );
        let _ = std::io::BufReader::new(stream).read_line(&mut String::new());
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while endpoint.socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    !endpoint.socket.exists()
}

#[test]
fn same_package_legacy_keeper_upgrades_before_the_first_lease_operation() {
    for verb in ["take", "renew", "release"] {
        let mut cli = LegacyCli::new();
        let fence = cli.fence.to_string();
        let args = match verb {
            "take" => vec!["claim", "take", "/workspace/a", "--json"],
            "renew" => vec!["claim", "renew", &fence, "--ttl", "1200", "--json"],
            _ => vec!["claim", "release", &fence, "--json"],
        };
        let doc = cli.json(&args, i32::from(verb == "take"));
        match verb {
            "take" => assert_eq!(
                doc["claim"]["outcome"], "conflict",
                "upgrade must preserve the existing lease"
            ),
            "renew" => assert_eq!(doc["claims"][0]["fence"], cli.fence),
            _ => assert_eq!(doc["released"], 1),
        }
        let seen = cli.legacy.take().unwrap().join().unwrap();
        let expected = if verb == "take" {
            vec!["take_claim"]
        } else if verb == "renew" {
            vec!["renew_claim", "list"]
        } else {
            vec!["release_claim", "list"]
        };
        assert_eq!(
            seen, expected,
            "legacy decoding rejection must negotiate before retrying a mutation"
        );
        assert!(
            retire_private_keeper(&cli.endpoint),
            "private replacement keeper did not exit"
        );
    }
}

#[test]
fn actual_cli_keeps_native_identity_and_fences_the_whole_lease() {
    let cli = CliStore::new();
    let take = [
        "claim",
        "take",
        "/workspace/a",
        "/workspace/b",
        "--ttl",
        "600",
        "--json",
    ];
    let first = cli.json("@749", &take, 0);
    assert_eq!(first["holder"], "@749");
    let fence = first["claim"]["claims"][0]["fence"].as_u64().unwrap();
    assert_eq!(first["claim"]["claims"][1]["fence"], fence);
    let receipt = fence.to_string();
    let other = cli.json("@730", &take, 1);
    assert_eq!(other["holder"], "@730");
    assert_eq!(other["claim"]["outcome"], "conflict");
    assert_eq!(cli.json("@749", &take, 1)["claim"]["outcome"], "conflict");
    let renewed = cli.json(
        "749",
        &["claim", "renew", &receipt, "--ttl", "1200", "--json"],
        0,
    );
    assert_eq!(renewed["holder"], "@749");
    assert_eq!(renewed["claims"].as_array().unwrap().len(), 2);
    let foreign = cli.json("@730", &["claim", "renew", &receipt, "--json"], 1);
    assert_eq!(foreign["error"]["code"], "claim_not_found");
    assert_eq!(
        cli.json("@730", &["claim", "release", &receipt, "--json"], 0)["released"],
        0
    );
    assert_eq!(
        cli.json("@749", &["claim", "release", &receipt, "--json"], 0)["released"],
        2
    );
    let next = cli.json("@749", &take, 0);
    assert!(next["claim"]["claims"][0]["fence"].as_u64().unwrap() > fence);
    assert_eq!(
        cli.json("@749", &["claim", "release", &receipt, "--json"], 0)["released"],
        0
    );
    assert_eq!(
        cli.json("@749", &["claim", "renew", &receipt, "--json"], 1)["error"]["code"],
        "claim_not_found"
    );
    assert_eq!(
        cli.json("@749", &["claim", "list", "--json"], 0)["claims"],
        next["claim"]["claims"]
    );
    assert_eq!(
        cli.json("@730", &["mail", "read", "--peek", "--json"], 0)["reader"],
        "@730"
    );
}

#[test]
fn unsafe_release_forms_and_malformed_native_context_fail_closed() {
    let cli = CliStore::new();
    let first = cli.json("@749", &["claim", "take", "/workspace/a", "--json"], 0);
    for args in [
        vec!["claim", "release", "--json"],
        vec!["claim", "release", "/workspace/a", "--json"],
        vec!["claim", "release", "0", "--json"],
    ] {
        assert!(!cli.run("@749", &args).status.success(), "{args:?}");
    }
    assert!(
        !cli.run(
            "not-a-pane",
            &["claim", "take", "/workspace/elsewhere", "--json"]
        )
        .status
        .success()
    );
    assert_eq!(
        cli.json("@749", &["claim", "list", "--json"], 0)["claims"],
        first["claim"]["claims"]
    );
}

fn send_mail(cli: &CliStore, to: &str, body: &str) -> Value {
    cli.json(
        "@749",
        &["mail", "send", to, "-t", "pr-1", "-m", body, "--json"],
        0,
    )
}

#[test]
fn mail_durable_ids_replay_across_cli_invocations_and_conflicts_keep_recovery() {
    let store = CliStore::new();
    let key = "12345678123456781234567812345678";
    let args = [
        "mail",
        "send",
        "@730",
        "-t",
        "retry",
        "-m",
        "once",
        "--operation-id",
        key,
        "--json",
    ];
    let first = store.json("@749", &args, 0);
    let replay = store.json("@749", &args, 0);
    assert_eq!(first["operation_id"], key);
    assert_eq!(first["scope"]["kind"], "work_store");
    assert_eq!(first["replayed"], false);
    assert_eq!(replay["replayed"], true);
    assert_eq!(first["mail"]["id"], replay["mail"]["id"]);
    let read = store.json("@730", &["mail", "read", "--all", "--peek", "--json"], 0);
    assert_eq!(read["mail"].as_array().unwrap().len(), 1);
    let conflict = store.json(
        "@749",
        &[
            "mail",
            "send",
            "@730",
            "-t",
            "retry",
            "-m",
            "changed",
            "--operation-id",
            key,
            "--json",
        ],
        1,
    );
    assert_eq!(conflict["error"]["code"], "mission_operation_conflict");
    assert_eq!(conflict["error"]["recovery"]["operation_id"], key);
    assert_eq!(conflict["error"]["recovery"]["scope"]["kind"], "work_store");
    let human = store.run(
        "@749",
        &[
            "mail",
            "send",
            "@730",
            "-t",
            "retry",
            "-m",
            "changed",
            "--operation-id",
            key,
        ],
    );
    assert_eq!(human.status.code(), Some(1));
    assert!(human.stdout.is_empty());
    let diagnostic = String::from_utf8_lossy(&human.stderr);
    assert!(
        diagnostic.contains("work-store operation id:"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains(key), "{diagnostic}");
    let invalid = store.json(
        "@749",
        &[
            "mail",
            "send",
            "@730",
            "-t",
            "retry",
            "-m",
            "bad",
            "--operation-id",
            "bad",
            "--json",
        ],
        2,
    );
    assert_eq!(invalid["error"]["code"], "usage_error");
}

#[test]
fn mail_is_stored_and_taken_once_for_the_agent_without_a_bell() {
    let cli = CliStore::new();
    let sent = send_mail(&cli, "opencode@730", "rebased");
    assert_eq!(sent["schema"], "phux.coord/v1");
    assert!(sent.get("bells").is_none(), "nothing rings a pane: {sent}");
    let wait = ["mail", "wait", "--timeout", "0", "--json"];
    let taken = cli.json("@730", &wait, 0);
    assert_eq!(taken["reader"], "@730");
    assert_eq!(taken["mail"][0]["body"], "rebased");
    let text = taken["text"].as_str().unwrap();
    assert!(text.contains("From @749 in thread pr-1"), "{text}");
    let again = cli.json("@730", &wait, 0);
    assert_eq!(again["mail"], serde_json::json!([]));
    assert!(again["text"].is_null());
    let inbox = cli.json("@730", &["mail", "read", "--peek", "--json"], 0);
    assert_eq!(inbox["mail"].as_array().unwrap().len(), 1, "still unread");
    assert!(
        cli.run(
            "@749",
            &["mail", "send", "@730", "-t", "x", "-m", "y", "--no-ring"]
        )
        .status
        .code()
        .is_some_and(|code| code != 0)
    );
}

#[test]
fn hook_context_prints_one_hook_reply_or_nothing() {
    let cli = CliStore::new();
    let hook = |event: &str| {
        let out = cli.run("@730", &["mail", "hook-context", "--event", event]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(hook("PostToolUse"), "", "no mail, no reply");
    send_mail(&cli, "@730", "first");
    let context: Value = serde_json::from_str(&hook("PostToolUse")).unwrap();
    assert_eq!(
        context["hookSpecificOutput"]["hookEventName"],
        "PostToolUse"
    );
    let text = context["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(text.contains("first"), "{text}");
    assert_eq!(hook("Stop"), "", "taken once");
    send_mail(&cli, "@730", "second");
    let stop: Value = serde_json::from_str(&hook("Stop")).unwrap();
    assert_eq!(stop["decision"], "block");
    assert!(stop["reason"].as_str().unwrap().contains("second"));
}

#[test]
fn another_hosts_pane_is_not_a_mail_address_here() {
    let cli = CliStore::new();
    let refused = cli.json(
        "@749",
        &[
            "mail",
            "wait",
            "--as",
            "mac/@730",
            "--timeout",
            "0",
            "--json",
        ],
        2,
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("this host"),
        "{refused}"
    );
}

#[test]
fn mail_that_cannot_be_handed_over_is_released_for_the_next_wait() {
    let cli = CliStore::new();
    send_mail(&cli, "@730", "keep me");
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let lost = command(cli.root.path(), "@730")
        .args(["mail", "wait", "--timeout", "0", "--json"])
        .stdout(writer)
        .output()
        .unwrap();
    assert_eq!(lost.status.code(), Some(1), "the reader was gone");
    let wait = ["mail", "wait", "--timeout", "0", "--json"];
    let taken = cli.json("@730", &wait, 0);
    assert_eq!(taken["mail"][0]["body"], "keep me", "released, then taken");
    let id = taken["mail"][0]["id"].as_str().unwrap().to_owned();
    let released = cli.json("@730", &["mail", "release", &id, "--json"], 0);
    assert_eq!(released["released"], 1);
    assert_eq!(cli.json("@730", &wait, 0)["mail"][0]["body"], "keep me");
}

#[test]
fn a_wait_bound_to_stdin_ends_with_its_caller_and_takes_nothing() {
    let cli = CliStore::new();
    let mut waiter = command(cli.root.path(), "@730")
        .args([
            "mail",
            "wait",
            "--timeout",
            "60",
            "--until-stdin-closes",
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(waiter.stdin.take());
    let started = Instant::now();
    let status = waiter.wait().unwrap();
    assert!(!status.success(), "abandoned, not answered");
    assert!(started.elapsed() < Duration::from_secs(10));
    send_mail(&cli, "@730", "for the live waiter");
    let taken = cli.json("@730", &["mail", "wait", "--timeout", "0", "--json"], 0);
    assert_eq!(taken["mail"][0]["body"], "for the live waiter");
}
