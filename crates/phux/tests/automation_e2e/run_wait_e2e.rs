//! Binary-level end-to-end tests for `phux run` and `phux wait` (phux-3rq).
//!
//! Unlike the pure unit tests in `phux-client` (which exercise the
//! sentinel parser and the wait condition in isolation), these drive the
//! REAL `phux` binary — built and handed to us by cargo at
//! `env!("CARGO_BIN_EXE_phux")` — against a REAL PTY-backed server. No
//! tmux, no mocks: a `phux server` child binds a private UDS, and each
//! verb runs as its own subprocess against that socket. They pin the
//! load-bearing process-exit contracts a consumer (an agent, a shell
//! `&&` chain) actually depends on:
//!
//!   * `run` mirrors the command's exit code into the process exit status
//!     (0 for success, 1 for `false`, an arbitrary code for a subshell).
//!   * `run --json` emits the stable `RunResult` contract.
//!   * `wait --until` exits 0 when the marker appears, 124 on timeout.
//!
//! Robustness notes:
//!   * The first server start can be slow (cold caches); we poll for the
//!     socket file with a generous deadline before driving any verb.
//!   * The server child is killed on guard drop, so a panicking assertion
//!     never leaks a daemon.
//!   * `--socket` is passed to EVERY verb so we never touch the user's
//!     real default socket, and the server is never auto-spawned.
//!   * The nonzero-mirror case uses `"(exit 7)"` — a POSIX subshell. This
//!     was verified empirically NOT to kill the session (a bare `exit 7`
//!     would terminate the shell and reap the pane); `phux ls` still
//!     lists the session afterward, and a subsequent `run` succeeds.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// Idle lifetime for this file's harness server, as a backstop UNDER the
/// `Drop` kill (ADR-0063). The guard is still the primary cleanup; it cannot
/// run if the test process is `SIGKILL`ed or the runner is reaped mid-job, and
/// what leaks then is a daemon holding a live PTY on a socket nobody will
/// ever look at again. Ten minutes is far longer than any gap between this
/// file's client connections, so it can only fire after the harness is gone.
const SERVER_IDLE_LIMIT_SECS: &str = "600";

/// Path to the freshly-built `phux` binary, injected by cargo for
/// integration tests in the same crate.
const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// The pre-seeded session name every test drives against.
const SESSION: &str = "work";

/// How long to wait for the server to bind its socket. Generous: the
/// very first build/start on a cold CI host is the slow case, and under a
/// loaded full-workspace run the spawned server competes for CPU (the
/// e2e-server nextest group serializes these tests to bound that load).
const SOCKET_DEADLINE: Duration = Duration::from_secs(30);

/// Poll cadence while waiting for the socket file to appear.
const SOCKET_POLL: Duration = Duration::from_millis(50);

/// Monotonic counter so concurrently-running tests (nextest runs each in
/// its own process, but `cargo test` shares one) never collide on a
/// socket path.
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A running `phux server`, killed when the guard drops so a failing
/// assertion never leaks a daemon.
struct ServerGuard {
    _process: common::ServerProcess,
    socket: PathBuf,
    // Held to keep the temp dir alive for the guard's lifetime.
    _dir: tempfile::TempDir,
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
        let dir = tempfile::tempdir().expect("create temp dir for socket");
        // Keep the path short: UDS paths have a ~104-char sun_path cap on
        // macOS, and a `tempdir()` path plus a long name can exceed it.
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = dir
            .path()
            .join(format!("e2e-{}-{n}.sock", std::process::id()));

        let child = Command::new(PHUX)
            .args(["server", "--session", SESSION, "--socket"])
            .arg(&socket)
            .args(["--exit-after-idle", SERVER_IDLE_LIMIT_SECS])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server");

        let guard = Self {
            _process: common::ServerProcess::from_child(child, socket.clone()),
            socket,
            _dir: dir,
        };
        guard.wait_for_socket();
        guard
    }

    /// Poll until the socket file exists or the deadline elapses.
    fn wait_for_socket(&self) {
        let deadline = Instant::now() + SOCKET_DEADLINE;
        while Instant::now() < deadline {
            if self.socket.exists() {
                return;
            }
            std::thread::sleep(SOCKET_POLL);
        }
        panic!(
            "phux server did not bind {} within {SOCKET_DEADLINE:?}",
            self.socket.display()
        );
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

/// Build a `phux <verb> --socket <sock> <rest...>` command, where
/// `args[0]` is the verb. `--socket` is injected right after the verb,
/// NOT appended: `run`/`wait`/`send-keys` use `trailing_var_arg`, so a
/// `--socket` placed after the positional command would be swallowed
/// into that command (and the verb would fall back to the user's real
/// default socket — verified the hard way). Verb-specific flags
/// (`--json`, `--until`, `--timeout`) must therefore also precede the
/// trailing positional in `args`.
fn phux_command(socket: &Path, args: &[&str]) -> Command {
    let (verb, rest) = args.split_first().expect("at least a verb");
    let mut command = Command::new(PHUX);
    command
        .arg(verb)
        .arg("--socket")
        .arg(socket)
        .args(rest)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Run a verb to completion, returning its raw exit code (the value a
/// shell would see in `$?`). Panics if the process was killed by a
/// signal rather than exiting normally.
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

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn run_mirrors_command_exit_codes() {
    let server = ServerGuard::start();
    assert_eq!(
        run_status(&server, &["run", SESSION, "true"]),
        0,
        "`phux run work true` should exit 0"
    );
    assert_eq!(
        run_status(&server, &["run", SESSION, "false"]),
        1,
        "`phux run work false` should mirror false's exit 1"
    );
    // A subshell preserves the interactive shell while exercising an
    // arbitrary nonzero status.
    assert_eq!(
        run_status(&server, &["run", SESSION, "(exit 7)"]),
        7,
        "`phux run work '(exit 7)'` should mirror the subshell's exit 7"
    );
    assert_eq!(
        run_status(&server, &["run", SESSION, "true"]),
        0,
        "session should survive a subshell `(exit 7)` and still run commands"
    );
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
fn wait_until_succeeds_when_marker_appears() {
    let server = ServerGuard::start();
    // Inject a marker into the pane, then wait for it. `--until` also
    // matches the command echo, which is fine for asserting exit 0.
    assert_eq!(
        run_status(
            &server,
            &["send-keys", SESSION, "echo WAIT_MARKER_XYZ", "Enter"],
        ),
        0,
        "send-keys should succeed"
    );
    assert_eq!(
        run_status(
            &server,
            &[
                "wait",
                SESSION,
                "--until",
                "WAIT_MARKER_XYZ",
                "--timeout",
                "5"
            ],
        ),
        0,
        "`phux wait --until` should exit 0 once the marker is visible"
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

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn wait_until_times_out_when_marker_never_appears() {
    let server = ServerGuard::start();
    assert_eq!(
        run_status(
            &server,
            &[
                "wait",
                SESSION,
                "--until",
                "STRING_THAT_NEVER_APPEARS_QZX",
                "--timeout",
                "1",
            ],
        ),
        124,
        "`phux wait` should exit 124 on timeout"
    );
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

// --- Selector grammar across run + send-keys (phux-n95) ----------------
//
// run/send-keys take the SAME `TARGET` grammar as snapshot/wait/kill, and
// resolve it client-side to the selected pane. The seeded server has one
// session ("work") with one window (index 0) holding one pane (local id 1),
// so every form below names that same pane; the test asserts each resolves
// (run mirrors `true`'s exit 0) rather than failing as "no such target".

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn run_accepts_successful_selector_forms() {
    let server = ServerGuard::start();
    for selector in ["work:0", "work:0.0", "@1"] {
        assert_eq!(
            run_status(&server, &["run", "--timeout", "15", selector, "true"]),
            0,
            "`phux run {selector}` should resolve the selector and mirror exit 0",
        );
    }
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
fn run_reports_no_such_target_for_unknown_session() {
    let server = ServerGuard::start();
    // Well-formed but nonexistent: parses fine, then misses at resolution.
    let (code, stderr) = run_status_and_stderr(
        &server.socket,
        &["run", "--timeout", "5", "no-such-session-qzx", "true"],
    );
    assert_eq!(code, 1, "an unresolvable target should exit 1");
    assert!(
        stderr.contains("no such target"),
        "expected a 'no such target' resolution diagnostic, got: {stderr}",
    );
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn send_keys_pane_selector_routes_to_that_pane() {
    let server = ServerGuard::start();
    // Address the pane explicitly (window 0, pane 0) and inject a marker.
    assert_eq!(
        run_status(
            &server,
            &[
                "send-keys",
                "work:0.0",
                "echo PANE_SELECTOR_MARK_QZX",
                "Enter"
            ],
        ),
        0,
        "send-keys with a pane selector should resolve and route",
    );
    // The marker must land on that very pane, observable via the same
    // selector form (here the opaque id of the seed pane).
    assert_eq!(
        run_status(
            &server,
            &[
                "wait",
                "@1",
                "--until",
                "PANE_SELECTOR_MARK_QZX",
                "--timeout",
                "5",
            ],
        ),
        0,
        "the marker should be visible on the pane the selector named",
    );
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn tag_round_trips_and_drives_the_hash_selector() {
    // phux-f8wi: tag a pane, read it back, then address it by `#tag`.
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

    // `tag ls --json` emits the documented schema_version-1 document
    // (agents.md §4.17) against a live server: `schema_version` 1 and one
    // row per Terminal, the canonical selector under `terminal` and the
    // full tag list under `tags` (phux-i0e8.8.5).
    let json_listed = run_stdout(&server, &["tag", "ls", SESSION, "--json"]);
    let doc: serde_json::Value =
        serde_json::from_str(&json_listed).expect("`phux tag ls --json` should emit valid JSON");
    assert_eq!(doc["schema_version"], 1, "document: {doc}");
    let terminals = doc["terminals"]
        .as_array()
        .expect("`terminals` should be an array");
    assert_eq!(
        terminals.len(),
        1,
        "one seed pane means one row; document: {doc}"
    );
    let row = &terminals[0];
    assert!(
        row["terminal"].as_str().is_some_and(|t| !t.is_empty()),
        "each row names its Terminal by canonical selector; document: {doc}"
    );
    let tags: Vec<&str> = row["tags"]
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
        doc.as_object().map(serde_json::Map::len),
        Some(2),
        "exactly the two documented top-level keys; document: {doc}"
    );

    // The `#tag` selector resolves the tagged pane — `wait` against it sees
    // the live shell (a wait on `#build` for a prompt-ish idle settles).
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

/// `phux snapshot --format html|vt` end to end: the real binary against a
/// real server, through libghostty-vt's own Formatter (D9, fallback rung
/// three — `docs/consumers/agents.md`). HTML must carry the pane's styled
/// text as a markup document; VT must write a non-empty raw byte capture;
/// `--json` must keep emitting the whole `ScreenState` document with
/// `rendered` populated instead of the raw capture.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn snapshot_format_html_writes_a_document() {
    let server = ServerGuard::start();
    // `%s%s ... SNAPSHOT_FORMAT _MARK` (two shell words, space-separated)
    // rather than the marker typed verbatim: the raw typed command line
    // is echoed to the pane the instant it's typed, before Enter is even
    // processed, so a marker present in the *source* text can make `wait
    // --until` match on the still-unexecuted command line rather than its
    // output. `printf` concatenates the two words with no space, so
    // "SNAPSHOT_FORMAT_MARK" (no space) only ever appears in the actual
    // executed output.
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
    // The marker was written bold-red (`\033[1;31m`); a faithful VT
    // capture must re-emit an SGR escape (`ESC [ ... m`) to reproduce
    // that styling, not just the plain characters.
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
