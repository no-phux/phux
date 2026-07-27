//! Binary-level end-to-end test for the ADR-0040 agent-identity record
//! (`phux-3ert`): `phux agent set` writes `phux.agent/v1` through the real
//! L3 `SET_METADATA` path, `phux agent show` reports from the record with
//! `agent_record` provenance (no heuristics), and `phux agent clear`
//! deletes it so the report falls back to the heuristic sources.
//!
//! Same harness discipline as `run_wait_e2e.rs`: a real `phux server`
//! child on a private UDS, each verb its own subprocess, guard-killed on
//! drop. Kept in its own file so the `just e2e` lane lists it explicitly.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Idle lifetime for this file's harness server, as a backstop UNDER the
/// `Drop` kill (ADR-0063). The guard is still the primary cleanup; it cannot
/// run if the test process is `SIGKILL`ed or the runner is reaped mid-job, and
/// what leaks then is a daemon holding a live PTY on a socket nobody will
/// ever look at again. Ten minutes is far longer than any gap between this
/// file's client connections, so it can only fire after the harness is gone.
const SERVER_IDLE_LIMIT_SECS: &str = "600";

/// Path to the freshly-built `phux` binary, injected by cargo.
const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// The pre-seeded session name the test drives against.
const SESSION: &str = "work";

/// How long to wait for the server to bind its socket (cold-start bound).
const SOCKET_DEADLINE: Duration = Duration::from_secs(30);

/// Poll cadence while waiting for the socket file to appear.
const SOCKET_POLL: Duration = Duration::from_millis(50);

/// Monotonic counter so concurrent tests never collide on a socket path.
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A running `phux server`, killed when the guard drops.
struct ServerGuard {
    child: Child,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl ServerGuard {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("create temp dir for socket");
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = dir
            .path()
            .join(format!("agent-{}-{n}.sock", std::process::id()));
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
            child,
            socket,
            _dir: dir,
        };
        guard.wait_for_socket();
        guard
    }

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

    /// Run `phux <args...> --socket <sock>` with `envs` capturing stdout.
    /// The verbs used here all take `--socket` as a per-verb flag (no
    /// trailing positional swallows it), so appending is safe.
    fn run(&self, args: &[&str], envs: &[(&str, &std::path::Path)]) -> String {
        let mut cmd = Command::new(PHUX);
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
        let out = Command::new(PHUX)
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

    // The report comes straight from the record — structured provenance,
    // no substring heuristics.
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
    assert_eq!(
        agent["sources"].as_array().map(Vec::len),
        Some(1),
        "no heuristic source may run while a record is declared"
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
