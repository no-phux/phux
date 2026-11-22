//! Failure UX end to end against the real binary: each once-silent failure
//! must stay loud.
//!
//! A broken config refuses the server start naming its path; one malformed
//! chord leaves the other bindings alive; a killed last pane explains the
//! ending; a `SIGKILL`ed server shows the reconnect indicator; and `status`,
//! `logs`, and `doctor` print the real server-log path. (The `config check`
//! and `--json` no-server contracts live in the configuration and workspace
//! suites.)

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use crate::common;

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{PtyAttach, ServerGuard};
use portable_pty::CommandBuilder;

/// The pre-seeded session every scenario drives against.
const SESSION: &str = "work";

/// How long to wait for a spawned server to bind its socket (cold-start
/// generous, mirroring `run_wait_e2e`).
const SOCKET_DEADLINE: Duration = Duration::from_secs(30);

/// Poll cadence for socket-file and child-exit waits.
const POLL: Duration = Duration::from_millis(50);

/// How long an attached client gets to reach a scripted state (exit,
/// output marker) before the test declares the scenario broken.
const CLIENT_DEADLINE: Duration = Duration::from_secs(20);

/// Per-scenario isolation: a private `HOME` plus XDG dirs.
struct Isolation {
    home: tempfile::TempDir,
    config: tempfile::TempDir,
    state: tempfile::TempDir,
}

impl Isolation {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().expect("isolated home"),
            config: tempfile::tempdir().expect("isolated config home"),
            state: tempfile::tempdir().expect("isolated state home"),
        }
    }

    /// Write `body` as this environment's `phux/config.toml`, returning
    /// the canonical path the loader will resolve.
    fn write_config(&self, body: &str) -> PathBuf {
        let dir = self.config.path().join("phux");
        std::fs::create_dir_all(&dir).expect("create phux config dir");
        let path = dir.join("config.toml");
        std::fs::write(&path, body).expect("write scenario config");
        path
    }

    /// The isolation table: private HOME/XDG dirs, the released `default`
    /// profile layout (a debug build would resolve `dev`), no overlay
    /// auto-listen (it would race the host's real server), and a missing
    /// `tailscale` so detection never reaches the operator's tailnet. Applied
    /// after `env_clear`, so no ambient `PHUX_*` from a live pane leaks in.
    fn env(&self) -> Vec<(&'static str, std::ffi::OsString)> {
        let mut env: Vec<(&'static str, std::ffi::OsString)> = ["PATH", "TMPDIR"]
            .into_iter()
            .filter_map(|key| std::env::var_os(key).map(|value| (key, value)))
            .collect();
        let home = self.home.path().as_os_str().to_owned();
        env.extend([
            ("HOME", home.clone()),
            ("XDG_CONFIG_HOME", self.config.path().as_os_str().to_owned()),
            ("XDG_STATE_HOME", self.state.path().as_os_str().to_owned()),
            ("XDG_CACHE_HOME", home.clone()),
            ("XDG_DATA_HOME", home.clone()),
            ("XDG_RUNTIME_DIR", home),
            ("PHUX_PROFILE", "default".into()),
            ("PHUX_NO_AUTO_LISTEN", "1".into()),
            ("PHUX_NO_UPDATE_CHECK", "1".into()),
            (
                "PHUX_TAILSCALE",
                self.home.path().join("no-such-tailscale").into_os_string(),
            ),
        ]);
        env
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env_clear().envs(self.env());
    }

    fn apply_pty(&self, cmd: &mut CommandBuilder) {
        cmd.env_clear();
        for (key, value) in self.env() {
            cmd.env(key, value);
        }
    }

    /// The canonical server-log path under this environment.
    fn server_log(&self) -> PathBuf {
        self.state.path().join("phux").join("server.log")
    }
}

/// Spawn the server inside `iso`; `SHELL=/bin/sh` keeps the seed pane free
/// of user rc noise.
fn start_server(iso: &Isolation) -> ServerGuard {
    ServerGuard::builder("fx")
        .env("SHELL", "/bin/sh")
        .start_with(|cmd| iso.apply(cmd))
}

impl ServerGuard {
    /// `phux <verb> --socket <sock> <rest...>` inside `iso`.
    fn cmd_in(&self, iso: &Isolation, args: &[&str]) -> Command {
        let (verb, rest) = args.split_first().expect("at least a verb");
        let mut cmd = common::phux_cmd(crate::runner::phux_bin());
        cmd.arg(verb)
            .arg("--socket")
            .arg(&self.socket)
            .args(rest)
            .stdin(Stdio::null());
        iso.apply(&mut cmd);
        cmd
    }
}

/// Whether this `ls --json` session entry is session `name` with a client
/// attached (the server's own count, bumped on `ATTACH`).
fn is_attached_session(session: &serde_json::Value, name: &str) -> bool {
    session["name"] == name
        && session["attached_clients"]
            .as_u64()
            .is_some_and(|count| count > 0)
}

/// Run a command to completion, returning `(exit_code, stdout, stderr)`.
fn run_captured(cmd: &mut Command) -> (i32, String, String) {
    let out = cmd.output().expect("run phux command");
    let code = out
        .status
        .code()
        .unwrap_or_else(|| panic!("phux terminated by signal: {:?}", out.status));
    (
        code,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A real `phux attach` TUI on a PTY inside `iso`, attached to `session`
/// (which need not exist). Killed on drop so a failing assertion never leaks
/// a client.
fn attach(server: &ServerGuard, iso: &Isolation, session: &str) -> PtyAttach {
    PtyAttach::start_with(&server.socket, &[session], (100, 24), |_config, cmd| {
        iso.apply_pty(cmd);
        cmd.env("SHELL", "/bin/sh");
        cmd.env("TERM", "xterm-256color");
        cmd.env("RUST_LOG", "off");
    })
}

/// Block until the server reports `client` attached to `name` (`ls --json`
/// `attached_clients`), a real barrier rather than a sleep.
fn wait_until_attached(client: &mut PtyAttach, server: &ServerGuard, iso: &Isolation, name: &str) {
    let deadline = Instant::now() + CLIENT_DEADLINE;
    while Instant::now() < deadline {
        assert!(
            client.is_running(),
            "attach client exited before the scenario ran; output:\n{}",
            client.painted(),
        );
        let (code, stdout, _stderr) = run_captured(&mut server.cmd_in(iso, &["ls", "--json"]));
        if code == 0
            && let Ok(doc) = serde_json::from_str::<serde_json::Value>(&stdout)
            && doc["sessions"].as_array().is_some_and(|sessions| {
                sessions
                    .iter()
                    .any(|session| is_attached_session(session, name))
            })
        {
            return;
        }
        std::thread::sleep(POLL);
    }
    panic!(
        "attach client never registered as attached to {name} within \
         {CLIENT_DEADLINE:?}; output:\n{}",
        client.painted(),
    );
}

#[test]
#[ignore = "spawns real phux processes; starves in the full parallel pool. Run via `just e2e`."]
fn broken_config_makes_server_start_loud() {
    let iso = Isolation::new();
    let config_path = iso.write_config("defaults = [ this is not toml\n");

    let dir = tempfile::tempdir().expect("socket tempdir");
    let socket = dir.path().join("fx-broken.sock");
    let mut cmd = common::phux_cmd(crate::runner::phux_bin());
    cmd.args(["server", "--session", SESSION, "--socket"])
        .arg(&socket)
        .args(["--exit-after-idle", "30"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    iso.apply(&mut cmd);
    // A guard, so a regressed server that starts anyway is still killed.
    let mut server =
        common::ServerProcess::from_child(cmd.spawn().expect("spawn phux server"), socket);

    // Poll: a regressed server would otherwise hang a blocking wait.
    let deadline = Instant::now() + SOCKET_DEADLINE;
    let status = loop {
        if let Some(status) = server.child_mut().try_wait().expect("server try_wait") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "server with a broken config did not exit within {SOCKET_DEADLINE:?} \
             (the audited silent-start regression)",
        );
        std::thread::sleep(POLL);
    };
    let mut stderr = String::new();
    server
        .child_mut()
        .stderr
        .take()
        .expect("piped stderr")
        .read_to_string(&mut stderr)
        .expect("read server stderr");

    assert!(
        !status.success(),
        "a broken config must refuse the start; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("cannot start"),
        "the refusal must be loud on stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(&config_path.display().to_string()),
        "the refusal must name the config path {}:\n{stderr}",
        config_path.display(),
    );
    assert!(
        stderr.contains("phux config check"),
        "the refusal must name its remedy `phux config check`:\n{stderr}"
    );
}

#[test]
#[ignore = "spawns real phux processes; starves in the full parallel pool. Run via `just e2e`."]
fn malformed_chord_keeps_detach_alive() {
    // One malformed chord ("q-") must not take every binding (detach
    // included) down with it.
    let iso = Isolation::new();
    iso.write_config("[keybindings.prefix-table]\n\"q-\" = \"kill-pane\"\nd = \"detach\"\n");
    let server = start_server(&iso);

    let mut client = attach(&server, &iso, SESSION);
    wait_until_attached(&mut client, &server, &iso, SESSION);
    // A pause for the client's own stdin reader: a keystroke written before
    // it exists is dropped, and nothing crosses the wire to wait on.
    std::thread::sleep(Duration::from_millis(500));

    // The default prefix (C-a) then `d`: detach must still resolve.
    client.send(b"\x01d");
    let status = client.wait_exit(CLIENT_DEADLINE);
    assert!(
        status.success(),
        "detach must survive one malformed chord (exit {:?}); output:\n{}",
        status.exit_code(),
        client.painted(),
    );
}

/// Attach is lookup-only on a running server: a missing name exits non-zero
/// and names `phux new NAME`, the verb that creates it.
#[test]
#[ignore = "spawns real phux processes; starves in the full parallel pool. Run via `just e2e`."]
fn attaching_a_missing_session_names_phux_new() {
    let iso = Isolation::new();
    let server = start_server(&iso);

    let mut client = attach(&server, &iso, "nosuch");
    let status = client.wait_exit(CLIENT_DEADLINE);
    assert!(
        !status.success(),
        "attaching a missing session must fail; output:\n{}",
        client.painted(),
    );
    client.await_painted(&["no session named \"nosuch\""], CLIENT_DEADLINE);
    client.await_painted(&["phux new nosuch"], CLIENT_DEADLINE);

    let (code, stdout, stderr) = run_captured(&mut server.cmd_in(&iso, &["ls", "--json"]));
    assert_eq!(code, 0, "ls must succeed; stderr:\n{stderr}");
    assert!(
        !stdout.contains("nosuch"),
        "a refused attach must not create the session:\n{stdout}"
    );
}

#[test]
#[ignore = "spawns real phux processes; starves in the full parallel pool. Run via `just e2e`."]
fn last_pane_death_surfaces_its_exit_status() {
    let iso = Isolation::new();
    let server = start_server(&iso);

    // An ephemeral session ends with its last pane, and the teardown line
    // names that pane's death.
    let (code, _stdout, stderr) =
        run_captured(&mut server.cmd_in(&iso, &["new", "--json", "--ephemeral", "-s", "solo"]));
    assert_eq!(code, 0, "new must succeed; stderr:\n{stderr}");
    let mut client = attach(&server, &iso, "solo");
    wait_until_attached(&mut client, &server, &iso, "solo");

    let (code, _stdout, stderr) =
        run_captured(&mut server.cmd_in(&iso, &["kill", "--yes", "solo:0.0"]));
    assert_eq!(code, 0, "kill must succeed; stderr:\n{stderr}");

    let status = client.wait_exit(CLIENT_DEADLINE);
    assert!(
        status.success(),
        "a last-pane death is an explained ending, not a client failure; output:\n{}",
        client.painted(),
    );
    client.await_painted(&["the last pane"], CLIENT_DEADLINE);
}

#[test]
#[ignore = "spawns real phux processes; starves in the full parallel pool. Run via `just e2e`."]
fn session_kill_detaches_with_its_explanation() {
    let iso = Isolation::new();
    let server = start_server(&iso);

    let mut client = attach(&server, &iso, SESSION);
    wait_until_attached(&mut client, &server, &iso, SESSION);

    // Killing the persistent session terminates its pane, removes it, and
    // deletes the session; a deleted session detaches its clients (R15), and
    // the teardown line explains the ending.
    let (code, _stdout, stderr) =
        run_captured(&mut server.cmd_in(&iso, &["kill", "--yes", SESSION]));
    assert_eq!(code, 0, "kill must succeed; stderr:\n{stderr}");

    let status = client.wait_exit(CLIENT_DEADLINE);
    assert!(
        status.success(),
        "a session kill is an explained ending, not a client failure; output:\n{}",
        client.painted(),
    );
    client.await_painted(&["the session was killed"], CLIENT_DEADLINE);
}

#[test]
#[ignore = "spawns real phux processes; starves in the full parallel pool. Run via `just e2e`."]
fn server_sigkill_shows_the_reconnect_indicator() {
    let iso = Isolation::new();
    let mut server = start_server(&iso);

    let mut client = attach(&server, &iso, SESSION);
    wait_until_attached(&mut client, &server, &iso, SESSION);

    server.sigkill();

    // The first line only, independent of the reconnect window's outcome.
    client.await_painted(&["lost the server connection"], CLIENT_DEADLINE);
}

#[test]
#[ignore = "spawns real phux processes; starves in the full parallel pool. Run via `just e2e`."]
fn status_logs_doctor_name_real_paths() {
    let iso = Isolation::new();
    let server = start_server(&iso);
    let server_log = iso.server_log().display().to_string();

    // `phux status` against the live server names the canonical log paths.
    let (code, stdout, stderr) = run_captured(&mut server.cmd_in(&iso, &["status"]));
    assert_eq!(
        code, 0,
        "status against a live server exits 0; stderr:\n{stderr}"
    );
    assert!(
        stdout.contains(&server_log),
        "status must name the server log {server_log}:\n{stdout}"
    );

    // Bare `phux logs` prints the inventory — every path, no server needed.
    let mut logs_cmd = common::phux_cmd(crate::runner::phux_bin());
    logs_cmd.arg("logs").stdin(Stdio::null());
    iso.apply(&mut logs_cmd);
    let (code, stdout, stderr) = run_captured(&mut logs_cmd);
    assert_eq!(code, 0, "the logs inventory exits 0; stderr:\n{stderr}");
    assert!(
        stdout.contains(&server_log),
        "logs must name the server log {server_log}:\n{stdout}"
    );

    // Doctor reads every ambient seam at once; the isolation keeps its exit
    // code about this setup, not the operator's install.
    let (code, stdout, stderr) = run_captured(&mut server.cmd_in(&iso, &["doctor"]));
    assert_eq!(
        code, 0,
        "doctor with a healthy setup exits 0; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains(&server.socket.display().to_string()),
        "doctor must name the socket it probed:\n{stdout}"
    );
    assert!(
        stdout.contains(&server_log),
        "doctor must name the server log {server_log}:\n{stdout}"
    );
}
