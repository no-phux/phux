//! Binary-level end-to-end test for the ADR-0040 agent-identity record
//! (`phux-3ert`): `phux agent set` writes `phux.agent/v1` through the real
//! L3 `SET_METADATA` path, `phux agent show` reports from the record with
//! `agent_record` authority plus detector provenance (no competing
//! heuristics), and `phux agent clear` deletes it so the report falls back to
//! the non-record sources.
//!
//! It also carries the phux-w7z2.26 join: the wrapper `phux agent
//! install-claude` actually GENERATES, driven through its own `--phux-hook`
//! entry point, against a live server on a pane painting a real Claude
//! permission dialog — asserting the record ends up carrying a state only
//! `rules/claude.toml` can produce. The two halves of that bug were
//! previously proven apart (a rendered-string assertion in `shim.rs`, a
//! hand-written identity-only `SET_METADATA` in
//! `phux-server/tests/agent_detect.rs`) and joined only by reasoning.
//!
//! Same harness discipline as `run_wait_e2e.rs`: a real `phux server`
//! child on a private UDS, each verb its own subprocess, guard-killed on
//! drop. Kept in its own file so the `just e2e` lane lists it explicitly.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};
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

/// Monotonic counter so concurrent tests never collide on a socket path.
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A running `phux server`, killed when the guard drops.
struct ServerGuard {
    _process: common::ServerProcess,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl ServerGuard {
    fn start() -> Self {
        Self::start_with_env(&[])
    }

    /// As [`Self::start`], with extra environment on the *server* child.
    ///
    /// The detector's tuning seams (`PHUX_AGENT_STARTUP_GRACE_MS`, …) are
    /// read once inside the server process, so they cannot be set from a
    /// client verb after the fact.
    fn start_with_env(envs: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().expect("create temp dir for socket");
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = dir
            .path()
            .join(format!("agent-{}-{n}.sock", std::process::id()));
        let mut cmd = Command::new(PHUX);
        cmd.args(["server", "--session", SESSION, "--socket"])
            .arg(&socket)
            .args(["--exit-after-idle", SERVER_IDLE_LIMIT_SECS]);
        for (key, value) in envs {
            cmd.env(key, value);
        }
        let child = cmd
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

    /// Create a pane running `command` and return its local Terminal id.
    ///
    /// `--socket` goes BEFORE the subcommand: `spawn`'s trailing positional
    /// would otherwise swallow it as part of the command line.
    fn spawn_pane(&self, command: &std::path::Path) -> u32 {
        let out = Command::new(PHUX)
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

    /// Sample `agent show` across `window` and return every distinct `state`
    /// observed, in first-seen order.
    ///
    /// A bounded negative assertion: the caller asserts on what did NOT
    /// appear, so the window has to be long enough to cover several detector
    /// ticks and the sampling has to be dense enough to catch a transient.
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

/// Write an executable fake Claude named `claude` into `dir`.
///
/// Two things are load-bearing and neither is the script's logic:
///
/// * **The name on disk.** `agent_detect::identify` resolves the kind from
///   the PTY foreground process group's argv, unwrapping runtime wrappers
///   (`sh`, `node`, …), so a `#!/bin/sh` script literally named `claude` is
///   what makes the shipped `rules/claude.toml` manifest apply. A title or a
///   screen can be forged; a process name is what the kernel says.
/// * **The screen.** It reproduces the shape Claude Code 2.1.207 actually
///   paints for a permission dialog, captured in
///   `phux-server/src/agent_detect/fixtures/claude/blocked_permission.txt`: a
///   horizontal rule with the dialog below it (the dialog REPLACES the input
///   box), carrying BOTH halves `prompt-permission-dialog` requires — the
///   "do you want to " stem and a numbered option line. The transcript line
///   above the rule is deliberate: `after-last-rule` must structurally
///   exclude it.
///
/// Then it holds, so the live screen keeps saying `blocked` for the whole
/// test rather than the pane being reaped mid-assertion.
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

/// Run the installed wrapper's own hook entry point, exactly as Claude Code
/// invokes it (`<shim> --phux-hook <event>`), with the environment a hook
/// process inherits from a Claude running inside a phux pane.
///
/// `PHUX_AGENT_PHUX_BIN` is deliberately NOT set: the wrapper must reach the
/// binary that installed it, which is the path baked in by `render_wrapper`.
fn run_hook(shim: &std::path::Path, event: &str, terminal_id: u32, socket: &std::path::Path) {
    let out = Command::new(shim)
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

/// phux-w7z2.26, joined end to end: the wrapper `phux agent install-claude`
/// GENERATES, run through its own hook entry point against a live server,
/// leaves the ADR-0046 detector armed on the pane it instruments.
///
/// This is the test the earlier fix could not produce, and its absence is why
/// the bead stayed open. The mechanism was pinned by a rendered-string
/// assertion in `commands::agent::shim` (the wrapper contains no `--state`)
/// and the consequence was pinned by
/// `phux-server/tests/agent_detect.rs::an_identity_only_set_gets_its_state_filled_in_by_the_detector`
/// — which uses a HAND-WRITTEN identity-only `SET_METADATA`. Nothing executed
/// the generated shim against a real server, so the two halves were joined by
/// reasoning. For a bug whose entire shape was "a shipped integration
/// silently disarms a shipped manifest", "the string looks right" is the same
/// class of evidence that let the original defect ship.
///
/// Three phases, each an independent claim:
///
/// 1. **The detector reaches the record on a shim pane.** After the
///    `SessionStart` hook, the pane converges on `blocked` — a state ONLY
///    `rules/claude.toml`'s `prompt-permission-dialog` rule can produce, and
///    one nothing in the shim can write.
/// 2. **A per-hook write does not clobber it (phux-w7z2.37).** The `blocked`
///    hook fires on every permission prompt and must reach `phux ask` and
///    nothing else; schema 2 wrote identity here too, and because
///    `SET_METADATA` replaces the record wholesale that published a
///    `blocked -> unknown` edge which `agent wait` reads as departure.
/// 3. **The counterfactual, so phase 1 cannot pass vacuously.** Declaring a
///    state the way schema 1 did stands the detector down on the same pane
///    with the same screen: the record sits on the declared value and never
///    returns to `blocked`. That IS the bug, reproduced, one `phux agent set`
///    away from the passing case.
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
    let install = Command::new(PHUX)
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
    // The behavior stamp `phux doctor` keys its staleness check on
    // (phux-w7z2.46). Deliberately NOT an assertion about `--state`: that is
    // `shim.rs`'s unit test, and repeating it here would let this test fail
    // on the rendered string before it ever reaches the live server — which
    // is the exact substitution of evidence this test exists to end.
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
