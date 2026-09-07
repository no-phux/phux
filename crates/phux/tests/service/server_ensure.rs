//! One-shot coordinator startup with real, isolated sockets and daemon cleanup.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::io::Read as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

struct Fixture {
    server: common::AutoSpawnedServer,
    socket: PathBuf,
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("isolated environment");
        let socket = dir.path().join("phux-ensure/phux.sock");
        std::fs::create_dir_all(socket.parent().expect("socket parent")).expect("runtime dir");
        std::fs::create_dir_all(dir.path().join("phux")).expect("config dir");
        Self {
            server: common::AutoSpawnedServer::new(PHUX, socket.clone()),
            socket,
            dir,
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(PHUX);
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path())
            .env("XDG_CONFIG_DIRS", self.dir.path())
            .env("XDG_STATE_HOME", self.dir.path())
            .env("XDG_RUNTIME_DIR", self.dir.path())
            .env("PHUX_PROFILE", "ensure")
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .env("RUST_LOG", "off")
            .env(phux::AUTO_SPAWN_IDLE_ENV, "30")
            .current_dir(self.dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn config(&self, body: &str) {
        std::fs::write(self.dir.path().join("phux/config.toml"), body).expect("config");
    }

    fn ensure(&mut self) -> Output {
        let output = bounded_output(self.command().args(["server", "--ensure"]));
        if output.status.success() {
            self.server.capture_pid();
        }
        output
    }

    fn status(&self) -> serde_json::Value {
        let output = bounded_output(self.command().args(["status", "--json"]));
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).expect("status document")
    }

    fn assert_cleaned_up(&self) {
        self.server.cleanup().expect("stop isolated daemon");
        assert!(!self.socket.exists(), "daemon socket must be removed");
    }
}

/// Kill and reap even a regressed helper that fails to enforce its own deadline.
fn bounded_output(cmd: &mut Command) -> Output {
    let child = cmd.spawn().expect("spawn CLI with no terminal");
    let mut guard = common::ServerProcess::from_child(child, PathBuf::new());
    assert!(
        guard.wait_for_exit(Duration::from_secs(15)).is_some(),
        "one-shot CLI hung"
    );
    let child = guard.child_mut();
    let status = child.try_wait().expect("wait").expect("exited");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_end(&mut stdout)
        .expect("read stdout");
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_end(&mut stderr)
        .expect("read stderr");
    Output {
        status,
        stdout,
        stderr,
    }
}

fn assert_success(output: &Output) {
    assert!(output.status.success(), "{output:?}");
    assert!(
        output.stdout.is_empty(),
        "no TUI or stdout payload: {output:?}"
    );
    assert!(!output.stderr.contains(&0x1b), "no terminal escapes");
}

#[test]
fn cold_profile_start_obeys_seed_policy_and_reuses_the_same_coordinator() {
    let mut fixture = Fixture::new();
    fixture.config("[defaults]\nsession-name-template = 'cockpit-seed'\nspawn-on-attach = 'printf seeded > seed-marker; exec /bin/sh'\n");
    assert_success(&fixture.ensure());
    UnixStream::connect(&fixture.socket).expect("accepting immediately after ensure exits");
    let first = fixture.status();
    assert_eq!(first["sessions"][0]["name"], "cockpit-seed", "{first}");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !fixture.dir.path().join("seed-marker").exists() {
        assert!(
            Instant::now() < deadline,
            "configured seed command did not run"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_success(&fixture.ensure());
    let second = fixture.status();
    assert_eq!(first["pid"], second["pid"]);
    assert_eq!(second["sessions"].as_array().expect("sessions").len(), 1);
    fixture.assert_cleaned_up();
}

#[test]
fn stale_socket_is_recovered_and_explicit_socket_overrides_environment() {
    let mut fixture = Fixture::new();
    drop(UnixListener::bind(&fixture.socket).expect("stale socket"));
    // Closing a UDS listener can briefly leave a connectable backlog on macOS.
    let deadline = Instant::now() + Duration::from_secs(3);
    while UnixStream::connect(&fixture.socket).is_ok() {
        assert!(Instant::now() < deadline, "fixture must stop accepting");
        std::thread::sleep(Duration::from_millis(25));
    }
    let other = fixture.dir.path().join("unused.sock");
    let output = bounded_output(
        fixture
            .command()
            .env("PHUX_SOCKET", &other)
            .arg("--socket")
            .arg(&fixture.socket)
            .args(["server", "--ensure"]),
    );
    fixture.server.capture_pid();
    assert_success(&output);
    UnixStream::connect(&fixture.socket).expect("recovered listener");
    assert!(!other.exists());
    fixture.assert_cleaned_up();
}

#[test]
fn socket_environment_selects_the_coordinator_without_a_flag() {
    let mut fixture = Fixture::new();
    let profile_socket = fixture.socket.clone();
    fixture.socket = fixture.dir.path().join("environment.sock");
    fixture.server = common::AutoSpawnedServer::new(PHUX, fixture.socket.clone());
    let output = bounded_output(
        fixture
            .command()
            .env("PHUX_SOCKET", &fixture.socket)
            .args(["server", "--ensure"]),
    );
    fixture.server.capture_pid();
    assert_success(&output);
    UnixStream::connect(&fixture.socket).expect("environment-selected coordinator");
    assert!(!profile_socket.exists());
    fixture.assert_cleaned_up();
}

#[test]
fn impossible_socket_path_fails_before_startup() {
    let fixture = Fixture::new();
    let socket = fixture.dir.path().join("x".repeat(200));
    let output = bounded_output(
        fixture
            .command()
            .args(["server", "--ensure", "--socket"])
            .arg(&socket),
    );
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(!socket.exists());
    assert!(!fixture.socket.exists());
}

#[test]
fn concurrent_ensures_elect_one_spawner() {
    let mut fixture = Fixture::new();
    let mut commands: Vec<_> = (0..4).map(|_| fixture.command()).collect();
    let outputs = std::thread::scope(|scope| {
        let workers: Vec<_> = commands
            .iter_mut()
            .map(|cmd| scope.spawn(|| bounded_output(cmd.args(["server", "--ensure"]))))
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().expect("ensure worker"))
            .collect::<Vec<_>>()
    });
    fixture.server.capture_pid();
    for output in &outputs {
        assert_success(output);
    }
    let log = std::fs::read_to_string(fixture.dir.path().join("phux-ensure/server.log"))
        .expect("daemon log");
    assert_eq!(log.matches("phux server listening on").count(), 1, "{log}");
    fixture.assert_cleaned_up();
}

#[test]
fn invalid_config_reports_startup_failure_and_log_path() {
    let mut fixture = Fixture::new();
    fixture.config("[defaults\n");
    let output = fixture.ensure();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("did not accept"), "{stderr}");
    assert!(stderr.contains("server.log"), "{stderr}");
    assert!(output.stdout.is_empty());
    assert!(UnixStream::connect(&fixture.socket).is_err());
}

#[test]
fn blocked_config_read_is_bounded_by_the_overall_deadline() {
    let mut fixture = Fixture::new();
    let status = Command::new("mkfifo")
        .arg(fixture.dir.path().join("phux/config.toml"))
        .status()
        .expect("mkfifo");
    assert!(status.success());
    let started = Instant::now();
    let output = fixture.ensure();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(started.elapsed() < Duration::from_secs(13));
    assert!(String::from_utf8_lossy(&output.stderr).contains("within 10s"));
    assert!(!fixture.socket.exists());
}

#[test]
fn ensure_rejects_foreground_options_instead_of_ignoring_them() {
    let fixture = Fixture::new();
    for args in [
        vec!["--session", "other"],
        vec!["--listen", "127.0.0.1:0"],
        vec!["--quic", "127.0.0.1:0"],
        vec!["--webtransport", "127.0.0.1:0"],
        vec!["--connect", "127.0.0.1:1"],
        vec!["--hub"],
        vec!["--exit-after-idle", "1"],
        vec!["--daemonize"],
        vec!["--seed-command", "true"],
        vec!["--resume", "3"],
    ] {
        let output = bounded_output(fixture.command().args(["server", "--ensure"]).args(args));
        assert_eq!(output.status.code(), Some(2), "{output:?}");
    }
    assert!(!fixture.socket.exists());
}
