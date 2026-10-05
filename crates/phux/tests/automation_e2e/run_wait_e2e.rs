//! `phux run` / `phux wait` against a real PTY-backed server: `run` mirrors
//! the command's exit code, `run --json` emits the `RunResult` contract, and
//! `wait --until` exits 0 on the marker and 124 on timeout. The nonzero case
//! uses a `(exit 7)` subshell so the pane's shell survives.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// The pre-seeded session name every test drives against.
const SESSION: &str = "work";

/// A running `phux server`, killed when the guard drops so a failing
/// assertion never leaks a daemon.
struct ServerGuard(common::ServerGuard);

impl std::ops::Deref for ServerGuard {
    type Target = common::ServerGuard;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A headless `phux watch --json` subprocess with its stdout decoded by a
/// background line reader. Killed on drop so assertion failures cannot leak it.
struct WatchGuard {
    child: Child,
    lines: Receiver<String>,
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl WatchGuard {
    /// Re-trigger output until the watcher observes one ordered dirty -> idle
    /// pair. The bounded receive is both the readiness probe and the retry
    /// cadence, so a slow subscription cannot lose the first trigger.
    fn trigger_and_wait_for_dirty_idle(&self, mut trigger: impl FnMut()) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_dirty = false;
        trigger();
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let retry_after = if saw_dirty {
                remaining
            } else {
                remaining.min(Duration::from_millis(100))
            };
            let line = match self.lines.recv_timeout(retry_after) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) if !saw_dirty => {
                    trigger();
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(err) => panic!("watch did not produce dirty -> idle: {err}"),
            };
            let event: serde_json::Value = serde_json::from_str(&line)
                .unwrap_or_else(|err| panic!("watch emitted invalid JSON {line:?}: {err}"));
            match event["event"].as_str() {
                Some("dirty") => saw_dirty = true,
                Some("idle") if saw_dirty => return,
                _ => {}
            }
        }
        panic!("watch did not produce dirty -> idle within 5s");
    }
}

impl ServerGuard {
    /// Spawn `phux server --session work --socket <unique>` detached
    /// from any terminal, then block until the socket file appears.
    fn start() -> Self {
        Self(common::ServerGuard::start("run-wait"))
    }

    fn cmd(&self, args: &[&str]) -> Command {
        phux_command(&self.socket, args)
    }

    /// Start the real CLI watcher without attaching to the pane. Its only
    /// server interaction is target resolution followed by event subscription.
    fn watch_json(&self) -> WatchGuard {
        let mut child = self
            .cmd(&["watch", "--json", SESSION])
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn phux watch --json");
        let stdout = child.stdout.take().expect("watch stdout pipe");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        WatchGuard { child, lines }
    }
}

/// `phux <verb> --socket <sock> <rest...>`: `--socket` goes right after the
/// verb because `run`/`wait`/`send-keys` take a trailing command that would
/// swallow it.
fn phux_command(socket: &Path, args: &[&str]) -> Command {
    let (verb, rest) = args.split_first().expect("at least a verb");
    let mut command = common::phux_cmd(crate::runner::phux_bin());
    command
        .arg(verb)
        .arg("--socket")
        .arg(socket)
        .args(rest)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Run a verb to completion and return its exit code.
fn run_status(server: &ServerGuard, args: &[&str]) -> i32 {
    let status = server
        .cmd(args)
        .stdout(Stdio::null())
        .status()
        .expect("run phux verb");
    status
        .code()
        .unwrap_or_else(|| panic!("phux {args:?} terminated by signal: {status:?}"))
}

/// Run a verb capturing stdout, asserting it exited 0, and return stdout.
fn run_stdout(server: &ServerGuard, args: &[&str]) -> String {
    let out = server
        .cmd(args)
        .output()
        .expect("run phux verb with output");
    assert!(
        out.status.success(),
        "phux {args:?} exited {:?}; stdout={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `run` mirrors the command's exit code (a subshell keeps the pane's shell
/// alive), resolves every selector form to the one seeded pane, and reports
/// an unknown session as `no such target`.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn run_mirrors_exit_codes_and_resolves_selectors() {
    let server = ServerGuard::start();
    for (command, code) in [("true", 0), ("false", 1), ("(exit 7)", 7), ("true", 0)] {
        assert_eq!(
            run_status(&server, &["run", SESSION, command]),
            code,
            "`phux run work {command:?}`"
        );
    }
    for selector in ["work:0", "work:0.0", "@1"] {
        assert_eq!(
            run_status(&server, &["run", "--timeout", "15", selector, "true"]),
            0,
            "`phux run {selector}` must resolve"
        );
    }
    let (code, stderr) = run_status_and_stderr(
        &server.socket,
        &["run", "--timeout", "5", "no-such-session-qzx", "true"],
    );
    assert_eq!(code, 1, "an unresolvable target exits 1");
    assert!(stderr.contains("no such target"), "{stderr}");
}

/// `send-keys` to a pane selector lands on that pane, and `wait --until` on
/// its `@id` exits 0 once the marker shows and 124 on timeout.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn wait_until_meets_the_marker_or_times_out() {
    let server = ServerGuard::start();
    let send = ["send-keys", "work:0.0", "echo WAIT_MARKER_XYZ", "Enter"];
    assert_eq!(run_status(&server, &send), 0);
    let wait = |marker: &str, timeout: &str| {
        run_status(
            &server,
            &["wait", "@1", "--until", marker, "--timeout", timeout],
        )
    };
    assert_eq!(wait("WAIT_MARKER_XYZ", "5"), 0);
    assert_eq!(wait("STRING_THAT_NEVER_APPEARS_QZX", "1"), 124);
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn run_json_reports_output_and_clean_exit() {
    let server = ServerGuard::start();
    // `--json` MUST precede the trailing command, or it is swallowed into
    // it (documented in the `run` help text).
    let stdout = run_stdout(&server, &["run", "--json", SESSION, "echo HELLO_E2E"]);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("`phux run --json` should emit valid JSON");
    assert_eq!(v["exit_code"], 0, "echo should exit 0; got {stdout}");
    assert_eq!(
        v["truncated"], false,
        "single-line echo output should not be truncated; got {stdout}"
    );
    let output = v["output"]
        .as_str()
        .expect("`output` field should be a string");
    assert!(
        output.contains("HELLO_E2E"),
        "captured output should contain the echoed marker; got {output:?}"
    );
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn headless_watch_json_receives_repeatable_dirty_idle_cycles() {
    let server = ServerGuard::start();
    let watch = server.watch_json();

    watch.trigger_and_wait_for_dirty_idle(|| {
        assert_eq!(
            run_status(
                &server,
                &["send-keys", SESSION, "printf WATCH_CYCLE_ONE", "Enter"],
            ),
            0,
        );
    });

    watch.trigger_and_wait_for_dirty_idle(|| {
        assert_eq!(
            run_status(
                &server,
                &["send-keys", SESSION, "printf WATCH_CYCLE_TWO", "Enter"],
            ),
            0,
        );
    });
}

/// Run a verb capturing exit code and stderr together — for asserting the
/// diagnostic on a selector that fails to parse or resolve.
fn run_status_and_stderr(socket: &Path, args: &[&str]) -> (i32, String) {
    let out = phux_command(socket, args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("run phux verb with stderr");
    let code = out
        .status
        .code()
        .unwrap_or_else(|| panic!("phux {args:?} terminated by signal: {:?}", out.status));
    (code, String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn run_rejects_malformed_selector_before_touching_server() {
    let dir = tempfile::tempdir().expect("create temp dir for absent socket");
    let absent_socket = dir.path().join("absent.sock");
    // A non-numeric pane index is a parse error, so the invalid-target
    // diagnostic must win even though no server exists at this socket.
    let (code, stderr) = run_status_and_stderr(&absent_socket, &["run", "work:0.x", "true"]);
    assert_eq!(code, 1, "a malformed selector should exit 1");
    assert!(
        stderr.contains("invalid target"),
        "expected an 'invalid target' parse diagnostic, got: {stderr}",
    );
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn tag_round_trips_and_drives_the_hash_selector() {
    let server = ServerGuard::start();

    // Tag the seed pane (resolving the whole session to its panes).
    assert_eq!(
        run_status(&server, &["tag", "add", SESSION, "build", "ci"]),
        0,
        "`phux tag add work build ci` should succeed",
    );

    // `tag ls` reflects the stored tags.
    let listed = run_stdout(&server, &["tag", "ls", SESSION]);
    assert!(
        listed.contains("build") && listed.contains("ci"),
        "`phux tag ls work` should list the tags; got: {listed}",
    );

    // The live `--json` document (shape pinned in `commands::tag` tests).
    let json_listed = run_stdout(&server, &["tag", "ls", SESSION, "--json"]);
    let doc: serde_json::Value =
        serde_json::from_str(&json_listed).expect("`phux tag ls --json` should emit valid JSON");
    assert_eq!(doc["schema_version"], 1, "document: {doc}");
    let terminals = doc["terminals"]
        .as_array()
        .expect("`terminals` should be an array");
    assert_eq!(terminals.len(), 1, "one seed pane; document: {doc}");
    let tags: Vec<&str> = terminals[0]["tags"]
        .as_array()
        .expect("`tags` should be an array")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert_eq!(
        tags,
        ["build", "ci"],
        "the stored tags come back sorted; document: {doc}"
    );

    assert_eq!(
        run_status(&server, &["tag", "ls", "#build"]),
        0,
        "`phux tag ls #build` should resolve via the tag index",
    );

    // Removing a tag drops it from the set.
    assert_eq!(run_status(&server, &["tag", "rm", "#build", "ci"]), 0);
    let after = run_stdout(&server, &["tag", "ls", SESSION]);
    assert!(
        after.contains("build") && !after.contains("ci"),
        "`ci` should be gone, `build` should remain; got: {after}",
    );

    // `#tag` drives a mutating verb: kill every Terminal tagged `build`.
    assert_eq!(
        run_status(&server, &["kill", "--yes", "#build"]),
        0,
        "`phux kill #build` should tear down the tagged Terminal",
    );
}

/// `snapshot --format html|vt` through libghostty's Formatter (D9): HTML is a
/// markup document with the text, VT a raw capture re-emitting SGR, and
/// `--json` carries it under `rendered`.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn snapshot_format_html_writes_a_document() {
    let server = ServerGuard::start();
    // Split so the marker appears only in the output, not the echoed line.
    assert_eq!(
        run_status(
            &server,
            &[
                "send-keys",
                SESSION,
                "printf '\\033[1;31m%s%s\\033[0m' SNAPSHOT_FORMAT _MARK",
                "Enter",
            ],
        ),
        0,
        "send-keys should deliver the styled marker",
    );
    assert_eq!(
        run_status(
            &server,
            &[
                "wait",
                SESSION,
                "--until",
                "SNAPSHOT_FORMAT_MARK",
                "--timeout",
                "5",
            ],
        ),
        0,
        "the marker should appear before snapshotting",
    );

    let html = run_stdout(&server, &["snapshot", "--format", "html", SESSION]);
    assert!(
        html.contains("SNAPSHOT_FORMAT_MARK"),
        "the HTML capture must carry the pane's text, got: {html}",
    );
    assert!(
        html.contains('<'),
        "`--format html` must write a markup document, got: {html}",
    );

    let vt = run_stdout(&server, &["snapshot", "--format", "vt", SESSION]);
    assert!(
        !vt.is_empty(),
        "`--format vt` must write a non-empty raw VT capture",
    );
    assert!(
        vt.contains("SNAPSHOT_FORMAT_MARK"),
        "the VT capture must carry the pane's text, got: {vt:?}",
    );
    assert!(
        vt.as_bytes().windows(2).any(|pair| pair == [0x1b, b'[']) && vt.contains('m'),
        "the VT capture must carry at least one SGR escape sequence, got: {vt:?}",
    );

    let json = run_stdout(
        &server,
        &["snapshot", "--json", "--format", "html", SESSION],
    );
    let doc: serde_json::Value =
        serde_json::from_str(&json).expect("--json --format html must emit valid JSON");
    assert_eq!(doc["rendered"]["format"], "html", "document: {doc}");
    assert!(
        doc["rendered"]["data"]
            .as_str()
            .is_some_and(|data| data.contains("SNAPSHOT_FORMAT_MARK")),
        "document: {doc}",
    );
}
