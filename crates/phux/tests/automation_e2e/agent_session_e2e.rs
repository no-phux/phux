//! Binary-level proof of the agent-session resource (ADR-0102..0104): the
//! real `phux` binary, one subprocess per verb, against a real server. Open,
//! emit, watch, log, show, and `ls` must all agree about one resource.
//!
//! The pane runs a script *named* `claude` that paints one clear-screen: the
//! detector only publishes for an identified occupant, and a blank screen
//! means every observed state came from the session's own record stream.
//! State ordering is asserted on `watch` (the event stream); the final level
//! is read back from `agent show` and must agree.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

const SESSION: &str = "work";

/// Shortened detector timers: the fake `claude` is identifiable at exec.
const TEST_STARTUP_GRACE_MS: &str = "200";
const TEST_RECHECK_MS: &str = "200";

/// Failure bound for one consequence to become visible through a verb.
const STEP_DEADLINE: Duration = Duration::from_secs(20);

const POLL: Duration = Duration::from_millis(50);

/// A running `phux server`, killed when the guard drops.
struct ServerGuard(common::ServerGuard);

impl std::ops::Deref for ServerGuard {
    type Target = common::ServerGuard;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ServerGuard {
    fn start() -> Self {
        Self(
            common::ServerGuard::builder("session")
                .env("PHUX_AGENT_STARTUP_GRACE_MS", TEST_STARTUP_GRACE_MS)
                .env("PHUX_AGENT_IDENTIFY_RECHECK_MS", TEST_RECHECK_MS)
                .start(),
        )
    }

    /// Run a verb and return its raw `Output`, success or not.
    fn try_run(&self, args: &[&str]) -> Output {
        self.cmd_global(args).output().expect("run phux verb")
    }

    /// Run a verb, asserting it succeeded, and return stdout.
    fn run(&self, args: &[&str]) -> String {
        let out = self.try_run(args);
        assert!(
            out.status.success(),
            "phux {args:?} exited {:?}; stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Run a verb whose stdout is one JSON document, and decode it.
    fn json(&self, args: &[&str]) -> serde_json::Value {
        let text = self.run(args);
        serde_json::from_str(&text)
            .unwrap_or_else(|err| panic!("phux {args:?} JSON ({err}): {text}"))
    }

    /// Decode the `--json` error document a refusing verb prints on stderr;
    /// stdout must stay empty.
    fn refusal(&self, args: &[&str], want_exit: i32) -> serde_json::Value {
        let out = self.try_run(args);
        assert_eq!(
            out.status.code(),
            Some(want_exit),
            "phux {args:?} must exit {want_exit}; stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stdout.is_empty(),
            "a refusal must not write to the document channel: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let text = String::from_utf8_lossy(&out.stderr);
        serde_json::from_str(&text)
            .unwrap_or_else(|err| panic!("phux {args:?} error JSON ({err}): {text}"))
    }

    /// Start a `phux watch --json` child against this server.
    fn watch(&self, target: &str, extra: &[&str]) -> common::WatchChild {
        common::WatchChild::start(
            Path::new(crate::runner::phux_bin()),
            &self.socket,
            target,
            extra,
            POLL,
            STEP_DEADLINE,
        )
    }

    /// Create a pane running `command` and return its local Terminal id.
    fn spawn_pane(&self, command: &Path) -> u32 {
        let mut cmd = self.cmd_global(&["spawn", "--json", "--"]);
        cmd.arg(command);
        let out = cmd.output().expect("run phux spawn");
        assert!(
            out.status.success(),
            "phux spawn exited {:?}; stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        let json: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("phux spawn --json");
        json["terminal_id"]
            .as_u64()
            .expect("spawn reports a terminal id")
            .try_into()
            .expect("terminal id fits u32")
    }

    /// The `resources` array of `phux ls --json`.
    fn resources(&self) -> Vec<serde_json::Value> {
        let listed = self.json(&["ls", "--json"]);
        listed["resources"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("`ls --json` must carry `resources`: {listed}"))
    }

    /// Poll `ls --json` until `resources` satisfies `want`, and return it.
    fn await_resources(
        &self,
        what: &str,
        want: impl Fn(&[serde_json::Value]) -> bool,
    ) -> Vec<serde_json::Value> {
        let end = Instant::now() + STEP_DEADLINE;
        loop {
            let resources = self.resources();
            if want(&resources) {
                return resources;
            }
            assert!(
                Instant::now() < end,
                "`ls --json` never showed {what} within {STEP_DEADLINE:?}; last: {resources:?}"
            );
            std::thread::sleep(POLL);
        }
    }

    /// Start `phux agent log SESSION --follow --json` and block until its
    /// bootstrap replay. That is the subscription barrier for the cascade
    /// test (a `watch` announces nothing when it subscribes), and the
    /// follower is a second observer of the close: it exits 0 when the
    /// session closes under it.
    fn follow_session(&self, session: &str, expect_bootstrap: usize) -> Follower {
        let out = tempfile::NamedTempFile::new().expect("follower stdout file");
        let path = out.path().to_path_buf();
        // Wrapped before the wait loop, so a panic on the deadline still
        // reaps the child rather than leaving it attached to the server.
        let follower = Follower {
            child: Some(
                self.cmd_global(&["agent", "log", session, "--follow", "--json"])
                    .stdout(out.reopen().expect("reopen follower stdout"))
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("spawn phux agent log --follow"),
            ),
        };
        let end = Instant::now() + STEP_DEADLINE;
        loop {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if text.lines().filter(|line| !line.is_empty()).count() >= expect_bootstrap {
                return follower;
            }
            assert!(
                Instant::now() < end,
                "`agent log --follow {session}` never replayed {expect_bootstrap} record(s) \
                 within {STEP_DEADLINE:?}; saw: {text:?}"
            );
            std::thread::sleep(POLL);
        }
    }

    /// `phux agent session open PARENT --provider P [--native-id ID] --json`,
    /// asserting the §4.19 document and returning the new resource id.
    fn session_open(&self, parent: &str, provider: &str, native_id: Option<&str>) -> String {
        let mut args = vec!["agent", "session", "open", parent, "--provider", provider];
        if let Some(native_id) = native_id {
            args.extend_from_slice(&["--native-id", native_id]);
        }
        args.push("--json");
        let opened = self.json(&args);
        assert_eq!(opened["schema_version"], 1, "{opened}");
        assert_eq!(opened["parent"], parent, "{opened}");
        assert_eq!(opened["provider"], provider, "{opened}");
        assert_eq!(
            opened["native_id"],
            native_id.map_or(serde_json::Value::Null, serde_json::Value::from),
            "{opened}"
        );
        opened["resource"]
            .as_str()
            .unwrap_or_else(|| panic!("open reports a resource id: {opened}"))
            .to_owned()
    }

    /// `phux agent emit TARGET --type T --data D --json`, returning the
    /// stamped header the server echoed.
    fn emit(&self, target: &str, event_type: &str, data: &str) -> serde_json::Value {
        let emitted = self.json(&[
            "agent", "emit", target, "--type", event_type, "--data", data, "--json",
        ]);
        assert_eq!(emitted["schema_version"], 1, "{emitted}");
        assert_eq!(emitted["resource"], target, "{emitted}");
        assert_eq!(emitted["type"], event_type, "{emitted}");
        emitted
    }
}

/// A running `phux agent log --follow` child, reaped either way: waited on
/// when the test asks for its verdict, killed if the test panics first.
struct Follower {
    child: Option<std::process::Child>,
}

impl Follower {
    /// Block until the follower ends on its own and return its exit code.
    fn verdict(mut self) -> Option<i32> {
        self.child
            .take()
            .expect("a follower is waited on exactly once")
            .wait()
            .expect("wait for the follower")
            .code()
    }
}

impl Drop for Follower {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Write an executable `claude` that paints one clear-screen and holds.
fn write_quiet_claude(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join("claude");
    std::fs::write(&path, "#!/bin/sh\nprintf '\\033[2J\\033[H'\nsleep 300\n")
        .expect("write quiet claude");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod quiet claude");
    path
}

/// The entry in `ls --json`'s `resources` for `id`, if any.
fn resource_named<'a>(
    resources: &'a [serde_json::Value],
    id: &str,
) -> Option<&'a serde_json::Value> {
    resources.iter().find(|entry| entry["id"] == id)
}

/// One session, opened and fed by the binary, read back through four verbs:
/// `session open` returns the §4.19 document and its own resource id; `emit`
/// stamps dense `seq` and server `ts_ms`; a watch started before the first emit
/// sees `working` then `done` on a pane that never painted; `log --tail 3`
/// replays the records; `ls --json` lists it under `resources` (never
/// `terminals`); `agent show` reports it under `agent_session`.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
#[allow(
    clippy::too_many_lines,
    reason = "one linear open-emit-observe scenario"
)]
#[expect(
    clippy::cognitive_complexity,
    reason = "one linear open-emit-observe scenario; every assert! scores as a branch"
)]
fn an_agent_session_is_opened_streamed_replayed_and_inventoried() {
    let fixtures = tempfile::tempdir().expect("create temp dir for the fixture");
    let claude = write_quiet_claude(fixtures.path());
    let server = ServerGuard::start();
    let pane_id = server.spawn_pane(&claude);
    let pane = format!("@{pane_id}");

    // --- 1. open ----------------------------------------------------------
    let session = server.session_open(&pane, "claude", Some("abc"));
    assert_ne!(session, pane, "a session is its own resource, not the pane");

    // The watch must be live (and the fixture identified) before the first
    // record.
    let watch = server.watch(&pane, &[]);
    watch.await_line("an initial agent_state line for the pane", |line| {
        line["event"] == "agent_state" && line["terminal"] == pane.as_str()
    });

    // --- 2. emit: prompt, tool_start, stop --------------------------------
    let prompt = server.emit(&session, "prompt", r#"{"chars":11}"#);
    assert_eq!(prompt["seq"], 1, "the first record on a session is seq 1");
    let started = prompt["ts_ms"].as_u64().expect("a server ts_ms");
    assert!(
        started > 1_600_000_000_000,
        "ts_ms is a wall clock: {prompt}"
    );

    // `working` has to be observed before `stop` retires it, or the two-edge
    // claim degenerates into "whatever the last state was".
    watch.await_agent_state("working");

    let tool = server.emit(&session, "tool_start", r#"{"tool_name":"Bash"}"#);
    assert_eq!(tool["seq"], 2, "{tool}");
    let stop = server.emit(&session, "stop", "{}");
    assert_eq!(stop["seq"], 3, "{stop}");
    assert!(
        stop["ts_ms"].as_u64().expect("a server ts_ms") >= started,
        "the server's clock does not run backwards across a session: {stop}"
    );

    // --- 3. the stream, and only the stream, moved the pane --------------
    let timeline = watch.await_agent_state("done");
    let working_at = common::first_index(&timeline, |line| {
        line["event"] == "agent_state" && line["state"] == "working"
    })
    .expect("a working edge");
    let done_at = common::first_index(&timeline, |line| {
        line["event"] == "agent_state" && line["state"] == "done"
    })
    .expect("a done edge");
    assert!(
        working_at < done_at,
        "the stream's order must reach the consumer in order: {timeline:?}"
    );
    // The pane never painted, so no screen rule can account for either edge.
    let painted = server.json(&["snapshot", &pane, "--json"]);
    let screen = painted["lines"]
        .as_array()
        .expect("snapshot lines")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<String>();
    assert!(
        screen.trim().is_empty(),
        "the fixture must paint nothing, or a screen rule could explain the states: {screen:?}"
    );

    // --- 4. the log replays what the server stamped -----------------------
    let log = server.json(&["agent", "log", &session, "--tail", "3", "--json"]);
    assert_eq!(log["schema_version"], 1, "{log}");
    assert_eq!(log["resource"], session.as_str(), "{log}");
    assert_eq!(log["parent"], pane.as_str(), "{log}");
    assert_eq!(log["provider"], "claude", "{log}");
    assert_eq!(log["native_id"], "abc", "{log}");
    let records = log["records"].as_array().expect("log records");
    assert_eq!(records.len(), 3, "--tail 3 returns three records: {log}");
    assert_eq!(
        records
            .iter()
            .map(|record| record["type"].as_str().unwrap_or("?"))
            .collect::<Vec<_>>(),
        vec!["prompt", "tool_start", "stop"],
        "{log}"
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record["seq"].as_u64().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec![1, 2, 3],
        "seq is dense and server-assigned: {log}"
    );
    assert_eq!(records[0]["data"]["chars"], 11, "{log}");
    assert_eq!(records[1]["data"]["tool_name"], "Bash", "{log}");
    for record in records {
        assert!(
            record["ts_ms"].as_u64().is_some_and(|ts| ts >= started),
            "every record carries the server's stamp: {record}"
        );
    }

    // --- 5. the inventory ------------------------------------------------
    let listed = server.json(&["ls", "--json"]);
    let resources = listed["resources"].as_array().expect("ls resources");
    let entry = resource_named(resources, &session)
        .unwrap_or_else(|| panic!("the session must be listed: {listed}"));
    assert_eq!(entry["kind"], "agent_session", "{listed}");
    assert_eq!(entry["parent"], pane.as_str(), "{listed}");
    let parent_entry = resource_named(resources, &pane)
        .unwrap_or_else(|| panic!("the pane must be listed: {listed}"));
    assert_eq!(parent_entry["kind"], "terminal", "{listed}");
    assert_eq!(parent_entry["parent"], serde_json::Value::Null, "{listed}");
    // The compatibility half: `terminals` stays the Terminal-kind inventory,
    // so a consumer that iterates it and snapshots each entry is unharmed by
    // a kind it has never heard of.
    let terminals = listed["terminals"].as_array().expect("ls terminals");
    assert!(terminals.iter().any(|id| id == pane.as_str()), "{listed}");
    assert!(
        !terminals.iter().any(|id| id == session.as_str()),
        "an agent session must never appear under `terminals`: {listed}"
    );

    // --- 6. the pane's own report ----------------------------------------
    let shown = server.json(&["agent", "show", &pane, "--json"]);
    let agent = &shown["agents"][0];
    assert_eq!(agent["terminal"], pane.as_str(), "{shown}");
    assert_eq!(
        agent["agent_session"]["resource"],
        session.as_str(),
        "{shown}"
    );
    assert_eq!(agent["agent_session"]["provider"], "claude", "{shown}");
    assert_eq!(agent["agent_session"]["native_id"], "abc", "{shown}");
    // The naming rule this key exists for: `session` in this document was
    // already the phux session name and still is.
    assert_eq!(agent["session"], SESSION, "{shown}");

    // The level agrees with the last edge: a blank pane's screen tick must not
    // overwrite the stream's `done`.
    assert_eq!(
        agent["state"], "done",
        "the stream's last word must survive the detector's screen tick: {shown}"
    );
    let sources = agent["sources"].as_array().expect("agent sources");
    let stream = sources
        .iter()
        .find(|source| source["kind"] == "stream")
        .unwrap_or_else(|| panic!("the top rung of the ladder must be named: {shown}"));
    assert_eq!(
        stream["observed"], "done",
        "the stream source carries the state the session derived: {shown}"
    );
}

/// `phux kill` on the pane closes the session (ADR-0104): a watch scoped to
/// the session sees `pane_closed`, the session leaves `ls`, and `agent log`
/// refuses the dead id.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn killing_the_parent_pane_cascades_the_session_closed() {
    let fixtures = tempfile::tempdir().expect("create temp dir for the fixture");
    let claude = write_quiet_claude(fixtures.path());
    let server = ServerGuard::start();
    let pane = format!("@{}", server.spawn_pane(&claude));
    let session = server.session_open(&pane, "claude", Some("cascade-01"));
    server.emit(&session, "prompt", r#"{"chars":3}"#);

    // The session has to be in the inventory before the kill, or its later
    // absence proves nothing.
    server.await_resources("the session", |resources| {
        resource_named(resources, &session).is_some()
    });

    // Order matters: the watch is started FIRST, then the follower, and the
    // follower's bootstrap line is what proves both are subscribed before
    // anything is killed. See `follow_session`.
    let watch = server.watch(&session, &[]);
    let follower = server.follow_session(&session, 1);

    server.run(&["kill", "--yes", &pane]);

    // The follower's own verdict on the close: a session that ends under a
    // `--follow` is a clean exit 0, not a broken pipe.
    assert_eq!(
        follower.verdict(),
        Some(0),
        "`agent log --follow` ends cleanly when its session closes"
    );

    let seen = watch.await_line("pane_closed for the session", |line| {
        line["event"] == "pane_closed"
    });
    let closed = seen
        .iter()
        .find(|line| line["event"] == "pane_closed")
        .expect("the pane_closed line");
    assert_eq!(
        closed["terminal"],
        session.as_str(),
        "the close must be scoped to the session, not to its parent: {closed}"
    );

    // The inventory loses both. They are polled together because the two
    // removals are not one snapshot apart: the session goes with the
    // cascade, the pane once its PTY is reaped.
    server.await_resources("the pane and its session both gone", |resources| {
        resource_named(resources, &session).is_none() && resource_named(resources, &pane).is_none()
    });

    // And the dead id refuses instead of replaying a ghost.
    let refusal = server.refusal(&["agent", "log", &session, "--json"], 1);
    assert_eq!(
        refusal["error"]["code"], "no_such_target",
        "a closed session is a target that no longer exists: {refusal}"
    );
}

/// Every producer refusal exits 2, writes nothing, and returns the
/// `record_invalid` / `no_agent_session` document (agents.md §2), including an
/// unknown `--type`.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn the_session_verbs_refuse_a_plain_pane_and_a_malformed_record() {
    let server = ServerGuard::start();
    let pane = format!("@{}", server.spawn_pane(Path::new("/bin/cat")));

    // A plain pane has no session, and both stream verbs say exactly that
    // rather than guessing at one.
    for verb in ["emit", "log"] {
        let args: Vec<&str> = if verb == "emit" {
            vec!["agent", "emit", &pane, "--type", "stop", "--json"]
        } else {
            vec!["agent", "log", &pane, "--json"]
        };
        let refusal = server.refusal(&args, 2);
        assert_eq!(
            refusal["error"]["code"], "no_agent_session",
            "`agent {verb}` on a plain pane: {refusal}"
        );
    }

    // With a live session the next refusals are about the record, not the
    // target.
    let session = server.session_open(&pane, "claude", None);
    let before = server.json(&["agent", "log", &session, "--json"]);
    assert!(
        before["records"].as_array().expect("records").is_empty(),
        "a new session's stream starts empty: {before}"
    );

    // `data` that parses as JSON but is not an object.
    let refusal = server.refusal(
        &[
            "agent", "emit", &session, "--type", "stop", "--data", "[1]", "--json",
        ],
        2,
    );
    assert_eq!(refusal["error"]["code"], "record_invalid", "{refusal}");

    // `data` that is not JSON at all.
    let refusal = server.refusal(
        &[
            "agent", "emit", &session, "--type", "stop", "--data", "not json", "--json",
        ],
        2,
    );
    assert_eq!(refusal["error"]["code"], "record_invalid", "{refusal}");

    // A `type` outside the closed v1 set: the same document shape as the two
    // above, with the closed set named in the remedy rather than in a usage
    // error a harness would have to tell apart from the rest.
    let refusal = server.refusal(
        &["agent", "emit", &session, "--type", "not_a_type", "--json"],
        2,
    );
    assert_eq!(refusal["error"]["code"], "record_invalid", "{refusal}");
    let remedy = refusal["remedy"].as_str().unwrap_or_default();
    assert!(
        remedy.contains("session_start") && remedy.contains("provider_raw"),
        "the refusal must name the closed set it is enforcing: {refusal}"
    );

    // An AgentSession is not a parent another session can hang off.
    let refusal = server.refusal(
        &[
            "agent",
            "session",
            "open",
            &session,
            "--provider",
            "claude",
            "--json",
        ],
        2,
    );
    assert_eq!(
        refusal["error"]["code"], "wrong_resource_kind",
        "only a Terminal parents a session: {refusal}"
    );

    // Nothing any of those refusals touched reached the stream.
    let after = server.json(&["agent", "log", &session, "--json"]);
    assert!(
        after["records"].as_array().expect("records").is_empty(),
        "a refused record must not reach the stream: {after}"
    );
}
