//! The ADR-0040 agent-identity record end to end: `agent set` writes
//! `phux.agent/v1` over `SET_METADATA`, `agent show` reports it with
//! `agent_record` authority, and `agent clear` falls back to the other
//! sources. Also drives the wrapper `agent install-claude` generates, through
//! its `--phux-hook` entry, against a pane painting a real Claude permission
//! dialog: the record must carry a state only `rules/claude.toml` produces.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// The pre-seeded session name the test drives against.
const SESSION: &str = "work";

/// Poll cadence while sampling `phux agent show`. Denser than the detector's
/// identified tick (~300 ms) so a transient record value cannot slip between
/// two samples of [`ServerGuard::states_over`].
const RECORD_POLL: Duration = Duration::from_millis(100);

/// The detector startup grace the shim test runs its server under (ADR-0046
/// point 6; production default 3 s). The fake Claude paints its dialog
/// immediately, so shortening it changes nothing about what is proven.
const TEST_STARTUP_GRACE_MS: &str = "200";

/// Ceiling for a detector verdict to reach the record and come back out of
/// `phux agent show`. A failure bound, not a timing gate.
const DETECT_DEADLINE: Duration = Duration::from_secs(20);

/// Bounded window for the "and it STAYS there" halves. Covers many detector
/// ticks at the identified cadence plus slack for a loaded pool.
const HOLD_WINDOW: Duration = Duration::from_secs(3);

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
        Self::start_with_env(&[])
    }

    /// As [`Self::start`], with extra environment on the server child (the
    /// detector's tuning seams are read once, inside the server).
    fn start_with_env(envs: &[(&str, &str)]) -> Self {
        Self(
            common::ServerGuard::builder("agent")
                .envs(envs.iter().copied())
                .start(),
        )
    }

    /// Run `phux <args...> --socket <sock>` with `envs` capturing stdout.
    /// The verbs used here all take `--socket` as a per-verb flag (no
    /// trailing positional swallows it), so appending is safe.
    fn run(&self, args: &[&str], envs: &[(&str, &std::path::Path)]) -> String {
        let mut cmd = common::phux_cmd(crate::runner::phux_bin());
        cmd.args(args).arg("--socket").arg(&self.socket);
        for (key, value) in envs {
            cmd.env(key, value);
        }
        let out = cmd.stdin(Stdio::null()).output().expect("run phux verb");
        assert!(
            out.status.success(),
            "phux {args:?} exited {:?}; stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Run `phux agent <args...> --socket <sock>` capturing stdout.
    /// `agent`'s subcommands take `--socket` as a per-verb flag (no
    /// trailing positional swallows it), so appending is safe here.
    fn agent(&self, args: &[&str]) -> String {
        let out = common::phux_cmd(crate::runner::phux_bin())
            .arg("agent")
            .args(args)
            .arg("--socket")
            .arg(&self.socket)
            .stdin(Stdio::null())
            .output()
            .expect("run phux agent verb");
        assert!(
            out.status.success(),
            "phux agent {args:?} exited {:?}; stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Create a pane running `command` and return its local Terminal id.
    ///
    /// `--socket` goes BEFORE the subcommand: `spawn`'s trailing positional
    /// would otherwise swallow it as part of the command line.
    fn spawn_pane(&self, command: &std::path::Path) -> u32 {
        let out = common::phux_cmd(crate::runner::phux_bin())
            .arg("--socket")
            .arg(&self.socket)
            .args(["spawn", "--json", "--"])
            .arg(command)
            .stdin(Stdio::null())
            .output()
            .expect("run phux spawn");
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

    /// `phux agent show TARGET --json`, decoded.
    fn agent_show(&self, target: &str) -> serde_json::Value {
        let shown = self.agent(&["show", target, "--json"]);
        serde_json::from_str(&shown)
            .unwrap_or_else(|err| panic!("agent show JSON ({err}): {shown}"))
    }

    /// Poll `agent show` until the record's `state` reads `want`, and return
    /// the whole document. Panics with the last reading on timeout.
    fn await_agent_state(&self, target: &str, want: &str, deadline: Duration) -> serde_json::Value {
        let end = Instant::now() + deadline;
        loop {
            let json = self.agent_show(target);
            if json["agents"][0]["state"] == want {
                return json;
            }
            assert!(
                Instant::now() < end,
                "{target} never reached state {want} within {deadline:?}; last: {json}"
            );
            std::thread::sleep(RECORD_POLL);
        }
    }

    /// Every distinct `state` seen while sampling `agent show` densely across
    /// `window`, in first-seen order: a bounded negative assertion.
    fn states_over(&self, target: &str, window: Duration) -> Vec<String> {
        let end = Instant::now() + window;
        let mut seen: Vec<String> = Vec::new();
        loop {
            let json = self.agent_show(target);
            let state = json["agents"][0]["state"]
                .as_str()
                .unwrap_or("<missing>")
                .to_owned();
            if !seen.contains(&state) {
                seen.push(state);
            }
            if Instant::now() >= end {
                return seen;
            }
            std::thread::sleep(RECORD_POLL);
        }
    }
}

/// The full declare/report/clear loop against a real server: the record
/// outranks heuristics while present and disappears cleanly on `clear`.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn agent_record_set_show_clear_roundtrip() {
    let server = ServerGuard::start();

    // Declare identity on the session's (single) pane.
    let confirmed = server.agent(&[
        "set",
        SESSION,
        "--name",
        "reviewer",
        "--kind",
        "claude",
        "--state",
        "blocked",
        "--session",
        "wave1",
    ]);
    assert!(
        confirmed.contains("\"name\":\"reviewer\""),
        "set must echo the confirmed record: {confirmed}"
    );

    // The report comes straight from the record. Detector provenance may
    // explain the record, but no identity or state heuristic may compete with
    // it.
    let shown = server.agent(&["show", SESSION, "--json"]);
    let json: serde_json::Value = serde_json::from_str(&shown).expect("agent show JSON");
    let agent = &json["agents"][0];
    assert_eq!(agent["agent"]["label"], "reviewer", "label from record");
    assert_eq!(agent["agent"]["kind"], "claude", "kind slug mapped");
    assert_eq!(agent["state"], "blocked", "state from record");
    assert_eq!(
        agent["sources"][0]["kind"], "agent_record",
        "provenance must be the structured record: {shown}"
    );
    let sources = agent["sources"].as_array().expect("sources array");
    assert!(
        sources.iter().skip(1).all(|source| {
            source["kind"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("detector_"))
        }),
        "only detector provenance may accompany the authoritative record: {shown}"
    );

    // Clear: the record is deleted and the report falls back to the
    // heuristic sources (whatever they infer, provenance is not the record).
    let cleared = server.agent(&["clear", SESSION]);
    assert!(
        cleared.trim_end().ends_with("\t-"),
        "clear must confirm the tombstone: {cleared:?}"
    );
    let shown = server.agent(&["show", SESSION, "--json"]);
    let json: serde_json::Value = serde_json::from_str(&shown).expect("agent show JSON");
    let sources = json["agents"][0]["sources"]
        .as_array()
        .expect("sources array");
    assert!(
        sources
            .iter()
            .all(|source| source["kind"] != "agent_record"),
        "after clear no source may claim the record: {shown}"
    );
}

/// phux-r82.10: `phux config agents` merges live `phux.agent/v1` records
/// into the manifest projection — a declared record overrides the static
/// manifest state (and propagates its derived attention), and clearing it
/// falls the row back to the declared manifest values.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn config_agents_projection_tracks_live_record() {
    let server = ServerGuard::start();

    // A configured plugin manifest declaring a static "codex" agent.
    let dir = tempfile::tempdir().expect("create temp config dir");
    let plugin_dir = dir.path().join("plugin");
    std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");
    let manifest = plugin_dir.join("phux-plugin.toml");
    std::fs::write(
        &manifest,
        concat!(
            "id = \"example.agent-tools\"\n",
            "name = \"Agent Tools\"\n",
            "version = \"0.1.0\"\n",
            "min_phux_version = \"0.0.2\"\n\n",
            "[[agents]]\n",
            "id = \"codex\"\n",
            "label = \"Codex\"\n",
            "state = \"idle\"\n",
            "attention = \"low\"\n",
        ),
    )
    .expect("write manifest");
    let xdg = dir.path().join("xdg");
    let config_dir = xdg.join("phux");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            "[[plugins]]\nmanifest = \"{}\"\nenabled = true\n",
            manifest.display()
        ),
    )
    .expect("write config");
    let envs: &[(&str, &std::path::Path)] = &[("XDG_CONFIG_HOME", xdg.as_path())];

    // Declare a live blocked codex record on the session's pane; the
    // projection must report the runtime state and its derived high
    // attention instead of the declared idle/low baseline.
    server.agent(&[
        "set", SESSION, "--name", "codex", "--kind", "codex", "--state", "blocked",
    ]);
    let live = server.run(&["config", "agents", "--json"], envs);
    let json: serde_json::Value = serde_json::from_str(&live).expect("config agents JSON");
    assert_eq!(json["schema_version"], 2);
    assert_eq!(json["live"], true, "server answered: {live}");
    let agent = &json["agents"][0];
    assert_eq!(agent["id"], "codex");
    assert_eq!(agent["state"], "blocked", "runtime overrides manifest");
    assert_eq!(agent["attention"], "high", "attention propagates: {live}");
    assert_eq!(agent["source"], "runtime");
    assert_eq!(agent["declared"]["state"], "idle");
    assert_eq!(agent["runtime"]["state"], "blocked");
    assert_eq!(agent["runtime"]["asked"], false);

    // Clear the record: the projection falls back to the declared values
    // even though the server is still live.
    server.agent(&["clear", SESSION]);
    let fallback = server.run(&["config", "agents", "--json"], envs);
    let json: serde_json::Value = serde_json::from_str(&fallback).expect("config agents JSON");
    assert_eq!(json["live"], true);
    let agent = &json["agents"][0];
    assert_eq!(agent["state"], "idle", "declared fallback: {fallback}");
    assert_eq!(agent["attention"], "low");
    assert_eq!(agent["source"], "manifest");
    assert_eq!(agent["runtime"], serde_json::Value::Null);
}

/// Write an executable fake Claude named `claude` (the kind is identified
/// from the foreground process name) that paints the permission-dialog shape
/// Claude Code actually draws (`phux-agent-rules` fixture
/// `claude/blocked_permission.txt`: transcript above a rule, the dialog stem and
/// numbered options below) and then holds.
fn write_fake_claude(dir: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join("claude");
    let script = concat!(
        "#!/bin/sh\n",
        "printf '\\033[2J\\033[H'\n",
        "echo 'some transcript output above the live chrome'\n",
        "echo ''\n",
        "printf '\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200",
        "\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200",
        "\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200",
        "\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\342\\224\\200\\n'\n",
        "echo ' Bash command'\n",
        "echo ''\n",
        "echo '   touch /tmp/probe.txt'\n",
        "echo ''\n",
        "echo ' Do you want to proceed?'\n",
        "printf ' \\342\\235\\257 1. Yes\\n'\n",
        "echo '   2. Yes, and always allow access'\n",
        "echo '   3. No'\n",
        "echo ''\n",
        "echo ' Esc to cancel'\n",
        "sleep 120\n",
    );
    std::fs::write(&path, script).expect("write fake claude");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake claude");
    path
}

/// Run the installed wrapper's hook entry point (`<shim> --phux-hook
/// <event>`) with a pane's environment. `PHUX_AGENT_PHUX_BIN` is unset: the
/// wrapper must reach the binary baked in at install.
fn run_hook(shim: &std::path::Path, event: &str, terminal_id: u32, socket: &std::path::Path) {
    let out = common::phux_cmd(shim)
        .args(["--phux-hook", event])
        .env("PHUX_TERMINAL_ID", terminal_id.to_string())
        .env("PHUX_SOCKET", socket)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|err| panic!("run the installed shim's {event} hook: {err}"));
    // The wrapper swallows `phux` failures on purpose — a broken control
    // plane must never break the user's Claude — so a zero exit proves only
    // that the hook path ran. What it actually did is read off the record.
    assert!(
        out.status.success(),
        "the {event} hook exited {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The wrapper `agent install-claude` generates, run through its hook entry
/// against a live server, leaves the ADR-0046 detector armed (the unit tests
/// only pinned the rendered string and a hand-written identity-only write):
///
/// 1. after `SessionStart` the pane reaches `blocked`, a state only
///    `rules/claude.toml` can produce;
/// 2. repeated `blocked` hooks do not clobber it (a wholesale record write
///    would publish `blocked -> unknown`, read by `agent wait` as departure);
/// 3. counterfactual: declaring a state, as the old shim did, stands the
///    detector down on the same screen.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn the_generated_claude_shim_leaves_the_detector_armed_on_a_live_pane() {
    let home = tempfile::tempdir().expect("create temp HOME");
    let bin = home.path().join("bin");
    std::fs::create_dir_all(&bin).expect("create bin dir");
    let fake_claude = write_fake_claude(&bin);
    let data = home.path().join("data");

    // Install through the real verb, so what runs below is the shipped
    // generator's output and not a fixture that resembles it.
    let install = common::phux_cmd(crate::runner::phux_bin())
        .args(["agent", "install-claude", "--shell", "bash", "--real"])
        .arg(&fake_claude)
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", &data)
        .stdin(Stdio::null())
        .output()
        .expect("run phux agent install-claude");
    assert!(
        install.status.success(),
        "install-claude exited {:?}; stderr={}",
        install.status.code(),
        String::from_utf8_lossy(&install.stderr)
    );
    let shim = data.join("phux").join("shims").join("claude");
    let installed = std::fs::read_to_string(&shim).expect("read the installed wrapper");
    // The staleness stamp `phux doctor` keys on; `--state` is `shim.rs`'s unit
    // test, not this one's.
    assert!(
        installed.contains("# phux-shim-schema: "),
        "the installed wrapper must carry its behavior stamp:\n{installed}"
    );

    let server =
        ServerGuard::start_with_env(&[("PHUX_AGENT_STARTUP_GRACE_MS", TEST_STARTUP_GRACE_MS)]);
    let terminal_id = server.spawn_pane(&fake_claude);
    let target = format!("@{terminal_id}");

    // --- 1. SessionStart: identity only, and the detector fills state in ---
    run_hook(&shim, "start", terminal_id, &server.socket);
    let json = server.await_agent_state(&target, "blocked", DETECT_DEADLINE);
    let agent = &json["agents"][0];
    assert_eq!(
        agent["sources"][0]["kind"], "agent_record",
        "the shim's write must be the authority the report reads: {json}"
    );
    assert_eq!(
        agent["agent"]["label"], "claude",
        "the shim's name survives the detector's state write: {json}"
    );
    assert_eq!(
        agent["agent"]["kind"], "claude",
        "and so does its kind: {json}"
    );

    // --- 2. The per-hook `blocked` write must not clobber the derived state -
    for _ in 0..3 {
        run_hook(&shim, "blocked", terminal_id, &server.socket);
    }
    let observed = server.states_over(&target, HOLD_WINDOW);
    assert_eq!(
        observed,
        vec!["blocked".to_owned()],
        "a per-hook record write resets the derived state to `unknown`, which \
         `agent wait` reads as the agent departing (phux-w7z2.37)"
    );

    // --- 3. The counterfactual: schema 1's declaration disarms the detector -
    server.agent(&[
        "set", &target, "--name", "claude", "--kind", "claude", "--state", "idle",
    ]);
    let observed = server.states_over(&target, HOLD_WINDOW);
    assert_eq!(
        observed,
        vec!["idle".to_owned()],
        "a declared state outranks the detector (ADR-0046 point 8), so the pane \
         sits on the declaration while its screen still shows a live permission \
         dialog — this is exactly what the shipped shim used to do on every hook, \
         and it is what phase 1 proves the generated wrapper no longer does"
    );
}

/// Write an executable `claude` that paints nothing and holds, so every
/// lifecycle edge must come from the session stream.
fn write_quiet_claude(dir: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join("claude");
    std::fs::write(&path, "#!/bin/sh\nprintf '\\033[2J\\033[H'\nsleep 120\n")
        .expect("write quiet claude");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod quiet claude");
    path
}

/// As [`run_hook`], with Claude's JSON payload on the hook's stdin — the
/// shape Claude Code actually hands a hook.
fn run_hook_with_payload(
    shim: &std::path::Path,
    event: &str,
    terminal_id: u32,
    socket: &std::path::Path,
    payload: &serde_json::Value,
) {
    use std::io::Write as _;

    let mut child = common::phux_cmd(shim)
        .args(["--phux-hook", event])
        .env("PHUX_TERMINAL_ID", terminal_id.to_string())
        .env("PHUX_SOCKET", socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("run the installed shim's {event} hook: {err}"));
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(payload.to_string().as_bytes())
        .expect("write the hook payload");
    let out = child.wait_with_output().expect("wait for the hook");
    assert!(
        out.status.success(),
        "the {event} hook exited {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

impl ServerGuard {
    /// Whether this server advertises `resource_kinds` — the same probe the
    /// generated wrapper runs, through the same verb.
    fn serves_resource_kinds(&self) -> bool {
        let status = self.run(&["status", "--json"], &[]);
        let json: serde_json::Value = serde_json::from_str(&status).expect("status JSON");
        json["features"]
            .as_array()
            .is_some_and(|features| features.iter().any(|f| f == "resource_kinds"))
    }

    /// `phux agent log TARGET --json`, decoded into its records. Accepts a
    /// JSON array, an object carrying a `records` array, or one record per
    /// line, so the assertion does not hinge on the verb's framing.
    fn agent_log(&self, target: &str) -> Vec<serde_json::Value> {
        let text = self.agent(&["log", target, "--json"]);
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(records) = value.as_array() {
                return records.clone();
            }
            if let Some(records) = value["records"].as_array() {
                return records.clone();
            }
        }
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|err| panic!("agent log record ({err}): {line}"))
            })
            .collect()
    }

    /// Poll the log until a record of `kind` appears and return the whole
    /// log. Panics with the last reading on timeout.
    fn await_record(&self, target: &str, kind: &str, deadline: Duration) -> Vec<serde_json::Value> {
        let end = Instant::now() + deadline;
        loop {
            let records = self.agent_log(target);
            if records.iter().any(|record| record["type"] == kind) {
                return records;
            }
            assert!(
                Instant::now() < end,
                "{target} never logged a `{kind}` record within {deadline:?}; last: {records:?}"
            );
            std::thread::sleep(RECORD_POLL);
        }
    }

    /// Poll the log until a `phux.transcript/v1` entry with `role` appears
    /// and return the whole log.
    fn await_transcript(
        &self,
        target: &str,
        role: &str,
        deadline: Duration,
    ) -> Vec<serde_json::Value> {
        let end = Instant::now() + deadline;
        loop {
            let records = self.agent_log(target);
            if records.iter().any(|record| is_transcript(record, role)) {
                return records;
            }
            assert!(
                Instant::now() < end,
                "{target} never logged a `{role}` transcript entry within {deadline:?}; last: {records:?}"
            );
            std::thread::sleep(RECORD_POLL);
        }
    }
}

fn is_transcript(record: &serde_json::Value, role: &str) -> bool {
    record["type"] == "provider_raw"
        && record["data"]["schema"] == "phux.transcript/v1"
        && record["data"]["provider"] == "claude"
        && record["data"]["entry"]["role"] == role
}

/// The last `phux.transcript/v1` entry with `role` in `records`.
fn transcript_entry(records: &[serde_json::Value], role: &str) -> serde_json::Value {
    records
        .iter()
        .rev()
        .find(|record| is_transcript(record, role))
        .map_or_else(
            || panic!("a `{role}` transcript entry: {records:?}"),
            |record| record["data"]["entry"].clone(),
        )
}

/// The generated wrapper, fed Claude's real hook payloads, drives an agent
/// session end to end: `SessionStart` opens it with Claude's `session_id`;
/// prompt, tool, ask, and stop hooks log records and the pane derives
/// `working`, `blocked`, then `done`; `SessionEnd` closes it. No prompt text,
/// tool input, or tool output may appear in a typed record; the conversation
/// rides `provider_raw` records in the `phux.transcript/v1` convention
/// (ADR-0156), and the transcript path never appears at all. States are read from a
/// `phux watch` because the detector's next screen tick supersedes a stream
/// edge within ~300 ms.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
#[allow(clippy::too_many_lines, reason = "one linear hook-by-hook scenario")]
fn the_generated_claude_shim_feeds_the_agent_session_stream() {
    const SESSION_ID: &str = "e2e-claude-session-0001";

    let home = tempfile::tempdir().expect("create temp HOME");
    let bin = home.path().join("bin");
    std::fs::create_dir_all(&bin).expect("create bin dir");
    let fake_claude = write_quiet_claude(&bin);
    let data = home.path().join("data");

    let install = common::phux_cmd(crate::runner::phux_bin())
        .args(["agent", "install-claude", "--shell", "bash", "--real"])
        .arg(&fake_claude)
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", &data)
        .stdin(Stdio::null())
        .output()
        .expect("run phux agent install-claude");
    assert!(
        install.status.success(),
        "install-claude exited {:?}; stderr={}",
        install.status.code(),
        String::from_utf8_lossy(&install.stderr)
    );
    let shim = data.join("phux").join("shims").join("claude");

    let server =
        ServerGuard::start_with_env(&[("PHUX_AGENT_STARTUP_GRACE_MS", TEST_STARTUP_GRACE_MS)]);
    // The wrapper takes its stream path only when the server serves
    // `AgentSession`; without it the test would prove nothing.
    assert!(
        server.serves_resource_kinds(),
        "this server must advertise `resource_kinds`"
    );
    let terminal_id = server.spawn_pane(&fake_claude);
    let target = format!("@{terminal_id}");
    // Live for the whole scenario: every state edge below is a published
    // record that the detector's next tick supersedes.
    let watch = common::WatchChild::start(
        std::path::Path::new(crate::runner::phux_bin()),
        &server.socket,
        &target,
        &[],
        RECORD_POLL,
        DETECT_DEADLINE,
    );
    watch.await_line("an initial agent_state line for the pane", |line| {
        line["event"] == "agent_state" && line["terminal"] == target.as_str()
    });
    let payload = |event: &str, extra: serde_json::Value| {
        let mut base = serde_json::json!({
            "session_id": SESSION_ID,
            "hook_event_name": event,
            "transcript_path": "/nonexistent/TRANSCRIPT-MARKER.jsonl",
            "cwd": "/tmp",
        });
        base.as_object_mut()
            .expect("object")
            .extend(extra.as_object().expect("object").clone());
        base
    };
    let assert_private = |records: &[serde_json::Value]| {
        let all = serde_json::to_string(records).expect("serialize log");
        assert!(
            !all.contains("TRANSCRIPT-MARKER"),
            "the transcript path must never reach the stream: {all}"
        );
        let typed: Vec<_> = records
            .iter()
            .filter(|record| record["type"] != "provider_raw")
            .collect();
        let text = serde_json::to_string(&typed).expect("serialize typed records");
        for marker in [
            "PROMPT-MARKER",
            "INPUT-MARKER",
            "OUTPUT-MARKER",
            "TRANSCRIPT-MARKER",
        ] {
            assert!(
                !text.contains(marker),
                "the stream must never carry `{marker}`: {text}"
            );
        }
    };

    // --- 1. SessionStart opens the session under the pane -----------------
    run_hook_with_payload(
        &shim,
        "start",
        terminal_id,
        &server.socket,
        &payload("SessionStart", serde_json::json!({ "source": "startup" })),
    );
    let records = server.await_record(&target, "session_start", DETECT_DEADLINE);
    assert_eq!(records[0]["type"], "session_start", "{records:?}");
    let shown = server.agent_show(&target);
    // The key is `agent_session`, not `session`: `session` in this document
    // was already the phux session name, and still is.
    let session = &shown["agents"][0]["agent_session"];
    assert_eq!(session["provider"], "claude", "{shown}");
    assert_eq!(session["native_id"], SESSION_ID, "{shown}");
    assert_eq!(shown["agents"][0]["session"], SESSION, "{shown}");

    // --- 2. UserPromptSubmit: a count, never the text; working ------------
    run_hook_with_payload(
        &shim,
        "working",
        terminal_id,
        &server.socket,
        &payload(
            "UserPromptSubmit",
            serde_json::json!({ "prompt": "hello PROMPT-MARKER world" }),
        ),
    );
    let records = server.await_record(&target, "prompt", DETECT_DEADLINE);
    let prompt = records
        .iter()
        .find(|r| r["type"] == "prompt")
        .expect("prompt record");
    assert_eq!(prompt["data"]["chars"], 25, "{prompt}");
    assert_private(&records);
    let user = transcript_entry(
        &server.await_transcript(&target, "user", DETECT_DEADLINE),
        "user",
    );
    assert_eq!(user["text"], "hello PROMPT-MARKER world", "{user}");
    assert_eq!(user["final"], true, "{user}");
    watch.await_agent_state("working");

    // --- 3. Tool records name the tool and nothing else -------------------
    run_hook_with_payload(
        &shim,
        "tool-start",
        terminal_id,
        &server.socket,
        &payload(
            "PreToolUse",
            serde_json::json!({
                "tool_name": "Bash",
                "tool_input": { "command": "echo INPUT-MARKER" },
                "tool_use_id": "toolu_e2e"
            }),
        ),
    );
    run_hook_with_payload(
        &shim,
        "tool-end",
        terminal_id,
        &server.socket,
        &payload(
            "PostToolUse",
            serde_json::json!({
                "tool_name": "Bash",
                "tool_input": { "command": "echo INPUT-MARKER" },
                "tool_response": "OUTPUT-MARKER",
                "tool_use_id": "toolu_e2e"
            }),
        ),
    );
    let records = server.await_record(&target, "tool_end", DETECT_DEADLINE);
    for kind in ["tool_start", "tool_end"] {
        let record = records
            .iter()
            .find(|r| r["type"] == kind)
            .unwrap_or_else(|| panic!("{kind} record: {records:?}"));
        assert_eq!(record["data"]["tool_name"], "Bash", "{record}");
        assert!(record["data"].get("tool_input").is_none(), "{record}");
    }
    assert_private(&records);
    let tool = transcript_entry(
        &server.await_transcript(&target, "tool", DETECT_DEADLINE),
        "tool",
    );
    assert_eq!(tool["id"], "toolu_e2e", "{tool}");
    assert_eq!(
        tool["tool"],
        serde_json::json!({
            "name": "Bash",
            "call_id": "toolu_e2e",
            "summary": "echo INPUT-MARKER",
            "status": "ok",
            "output": "OUTPUT-MARKER"
        }),
        "{tool}"
    );

    // --- 4. PermissionRequest: ask, and blocked ---------------------------
    run_hook_with_payload(
        &shim,
        "blocked",
        terminal_id,
        &server.socket,
        &payload(
            "PermissionRequest",
            serde_json::json!({
                "tool_name": "Bash",
                "tool_input": { "command": "rm INPUT-MARKER" },
                "message": "Claude wants to run: rm INPUT-MARKER"
            }),
        ),
    );
    let records = server.await_record(&target, "ask", DETECT_DEADLINE);
    assert_private(&records);
    watch.await_agent_state("blocked");

    // --- 5. Stop: done ----------------------------------------------------
    run_hook_with_payload(
        &shim,
        "done",
        terminal_id,
        &server.socket,
        &payload(
            "Stop",
            serde_json::json!({ "last_assistant_message": "OUTPUT-MARKER" }),
        ),
    );
    let records = server.await_record(&target, "stop", DETECT_DEADLINE);
    assert_private(&records);
    let reply = transcript_entry(&records, "assistant");
    assert_eq!(reply["text"], "OUTPUT-MARKER", "{reply}");
    let seq_of = |pred: &dyn Fn(&serde_json::Value) -> bool| {
        records.iter().find(|r| pred(r)).map(|r| r["seq"].as_u64())
    };
    assert!(
        seq_of(&|r| r["data"]["entry"]["role"] == "assistant") < seq_of(&|r| r["type"] == "stop"),
        "the reply lands before the turn's stop: {records:?}"
    );
    watch.await_agent_state("done");
    // The whole ordered ladder, on one stream: the hooks drove the pane
    // through working, blocked and done in that order, from a screen that
    // never painted a character.
    let timeline = watch.seen();
    let at = |state: &str| {
        common::first_index(&timeline, |line| {
            line["event"] == "agent_state" && line["state"] == state
        })
        .unwrap_or_else(|| panic!("a {state} edge: {timeline:?}"))
    };
    assert!(
        at("working") < at("blocked") && at("blocked") < at("done"),
        "the hook order must reach the consumer in order: {timeline:?}"
    );

    // --- 6. SessionEnd: session_end, and the session is closed ------------
    run_hook_with_payload(
        &shim,
        "clear",
        terminal_id,
        &server.socket,
        &payload(
            "SessionEnd",
            serde_json::json!({ "reason": "prompt_input_exit" }),
        ),
    );
    // The stream wrapper ends the session without `agent clear`: the session
    // leaves the report and inventory, and the record goes back to the detector.
    let end = Instant::now() + DETECT_DEADLINE;
    loop {
        let shown = server.agent_show(&target);
        let listed = server.run(&["ls", "--json"], &[]);
        let inventory: serde_json::Value =
            serde_json::from_str(&listed).unwrap_or_else(|err| panic!("ls JSON ({err}): {listed}"));
        let gone_from_report = shown["agents"][0]["agent_session"].is_null();
        let gone_from_inventory = inventory["resources"].as_array().is_some_and(|resources| {
            resources
                .iter()
                .all(|entry| entry["kind"] != "agent_session")
        });
        if gone_from_report && gone_from_inventory {
            break;
        }
        assert!(
            Instant::now() < end,
            "SessionEnd must close the session; last report: {shown}; inventory: {inventory}"
        );
        std::thread::sleep(RECORD_POLL);
    }
    // And the closed session's stream is no longer readable through the pane.
    let out = common::phux_cmd(crate::runner::phux_bin())
        .arg("agent")
        .args(["log", &target, "--json", "--socket"])
        .arg(&server.socket)
        .stdin(Stdio::null())
        .output()
        .expect("run phux agent log");
    assert!(
        !out.status.success(),
        "`agent log` on a pane whose session ended must refuse: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// Write an executable named `name` that sets the OSC 0/2 title `title`,
/// paints nothing else, and holds. With an empty screen, any state the server
/// derives for it comes from a `title`-scoped rule alone.
fn write_titled_agent(dir: &std::path::Path, name: &str, title: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join(name);
    let script = format!("#!/bin/sh\nprintf '\\033[2J\\033[H\\033]2;{title}\\007'\nsleep 120\n");
    std::fs::write(&path, script).expect("write titled agent");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod titled agent");
    path
}

/// phux-4uzr: when the server derives a pane's state from a `title`-scoped
/// rule alone, `agent show` and `agent explain` must replay the manifest
/// against the same OSC title the detector read. They once read
/// `GET_STATE`'s `ResourceInfo::title`, which is the user-set title and
/// always empty here, so the explanation said nothing matched while the
/// record reported `blocked`.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn a_title_only_rule_is_explained_with_the_title_the_detector_read() {
    // OMP's `title-attention` rule (`^π !`) is title-only; the screen is blank.
    const TITLE: &str = "π ! e2e";

    let home = tempfile::tempdir().expect("create temp dir");
    let fake_omp = write_titled_agent(home.path(), "omp", TITLE);
    let server =
        ServerGuard::start_with_env(&[("PHUX_AGENT_STARTUP_GRACE_MS", TEST_STARTUP_GRACE_MS)]);
    let terminal_id = server.spawn_pane(&fake_omp);
    let target = format!("@{terminal_id}");

    let json = server.await_agent_state(&target, "blocked", DETECT_DEADLINE);
    let agent = &json["agents"][0];
    assert_eq!(agent["agent"]["kind"], "omp", "{json}");
    assert_eq!(
        agent["title"], TITLE,
        "show must report the live OSC title: {json}"
    );
    let sources = agent["sources"].as_array().expect("sources array");
    let rule = sources
        .iter()
        .find(|source| source["kind"] == "detector_rule")
        .unwrap_or_else(|| panic!("a detector_rule source must name the title rule: {json}"));
    assert_eq!(rule["rule"], "title-attention", "{json}");
    assert_eq!(rule["region"], "title", "{json}");
    assert!(
        sources
            .iter()
            .all(|source| source["kind"] != "detector_fallback"),
        "the explanation must not claim nothing matched: {json}"
    );

    let explained = server.agent(&["explain", &target]);
    assert!(
        explained.contains("rule `title-attention` matched the `title` region"),
        "explain must name the title rule: {explained}"
    );
}

/// Write an executable that raises the ADR-0035 `phux-ask` title sentinel
/// for `deploy`, then writes the first line it reads to `answer_file`.
fn write_asker(dir: &std::path::Path, answer_file: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join("asker");
    let script = format!(
        "#!/bin/sh\nprintf '\\033]2;phux-ask[deploy]:Deploy to prod??s=Yes|No\\007'\n\
         IFS= read -r answer\nprintf '%s\\n' \"$answer\" > '{}'\nsleep 120\n",
        answer_file.display()
    );
    std::fs::write(&path, script).expect("write asker");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod asker");
    path
}

/// phux-4uzr's sibling: `agent answer` and `config agents` find the
/// `phux-ask` sentinel in the live OSC title. Reading `GET_STATE`'s user-set
/// title instead, `answer` refused every live ask with `no_active_ask` and
/// `config agents` never reported `asked`.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn agent_answer_and_config_agents_read_the_live_ask_title() {
    let home = tempfile::tempdir().expect("create temp dir");
    let answer_file = home.path().join("answer.txt");
    let asker = write_asker(home.path(), &answer_file);
    let server = ServerGuard::start();
    let terminal_id = server.spawn_pane(&asker);
    let target = format!("@{terminal_id}");

    // A barrier on the title itself, read through the verb under test's own
    // source: the asker has set it once `snapshot` reports it.
    let end = Instant::now() + DETECT_DEADLINE;
    loop {
        let shot = server.run(&["snapshot", &target, "--json"], &[]);
        let screen: serde_json::Value = serde_json::from_str(&shot).expect("snapshot JSON");
        if screen["title"]
            .as_str()
            .is_some_and(|title| title.starts_with("phux-ask[deploy]"))
        {
            break;
        }
        assert!(
            Instant::now() < end,
            "the asker never set its title: {screen}"
        );
        std::thread::sleep(RECORD_POLL);
    }

    // `config agents`: a recorded pane whose live title asks reads `asked`.
    let config = tempfile::tempdir().expect("create temp config dir");
    let manifest = config.path().join("phux-plugin.toml");
    std::fs::write(
        &manifest,
        concat!(
            "id = \"example.asker\"\n",
            "name = \"Asker\"\n",
            "version = \"0.1.0\"\n",
            "min_phux_version = \"0.0.2\"\n\n",
            "[[agents]]\n",
            "id = \"asker\"\n",
            "label = \"Asker\"\n",
        ),
    )
    .expect("write manifest");
    let xdg = config.path().join("xdg");
    std::fs::create_dir_all(xdg.join("phux")).expect("create config dir");
    std::fs::write(
        xdg.join("phux").join("config.toml"),
        format!(
            "[[plugins]]\nmanifest = \"{}\"\nenabled = true\n",
            manifest.display()
        ),
    )
    .expect("write config");
    server.agent(&["set", &target, "--name", "asker", "--kind", "asker"]);
    let live = server.run(
        &["config", "agents", "--json"],
        &[("XDG_CONFIG_HOME", xdg.as_path())],
    );
    let json: serde_json::Value = serde_json::from_str(&live).expect("config agents JSON");
    assert_eq!(
        json["agents"][0]["runtime"]["asked"], true,
        "the live phux-ask title must surface as asked: {live}"
    );

    // `agent answer`: the validated choice is typed into the asking pane.
    let answered = server.agent(&["answer", &target, "--id", "deploy", "--choice", "1"]);
    assert!(
        answered.contains("answered deploy"),
        "answer must confirm delivery: {answered}"
    );
    let end = Instant::now() + DETECT_DEADLINE;
    loop {
        if let Ok(text) = std::fs::read_to_string(&answer_file)
            && text.ends_with('\n')
        {
            assert_eq!(text, "Yes\n", "the first suggestion is the answer");
            break;
        }
        assert!(Instant::now() < end, "the asker never received the answer");
        std::thread::sleep(RECORD_POLL);
    }
}

/// ADR-0075 point 5 on `agent start`: it types the launch line over
/// acknowledged input, so a `%name` whose record has the withdrawn shape (a
/// `kind`, `state: unknown`) is refused as `agent_withdrawn` before anything
/// is typed, as `send-keys`, `paste`, and `agent prompt` refuse it.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn agent_start_refuses_a_withdrawn_name() {
    let server = ServerGuard::start();
    // `set` without `--state` leaves `unknown`: with a kind, the withdrawn shape.
    server.agent(&["set", SESSION, "--name", "build", "--kind", "codex"]);
    // The repo's example plugin config supplies a `codex` integration, so the
    // launch plan resolves the same on every host and the guard is what refuses.
    let config = crate::runner::manifest_dir()
        .join("../../examples/plugins/agent-tools/config")
        .canonicalize()
        .expect("example plugin config");
    let out = common::phux_cmd(crate::runner::phux_bin())
        .env("XDG_CONFIG_HOME", &config)
        .args([
            "agent", "start", "--json", "--kind", "codex", "--target", "%build",
        ])
        .arg("--socket")
        .arg(&server.socket)
        .arg("reviewer")
        .stdin(Stdio::null())
        .output()
        .expect("run phux agent start");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr={stderr}");
    let error: serde_json::Value = stderr
        .lines()
        .find_map(|line| serde_json::from_str(line).ok())
        .unwrap_or_else(|| panic!("no JSON error document: {stderr}"));
    assert_eq!(error["error"]["code"], "agent_withdrawn", "{stderr}");
}
