//! One-shot coordinator startup with real, isolated sockets and daemon cleanup.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::io::Read as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    server: common::AutoSpawnedServer,
    socket: PathBuf,
    dir: tempfile::TempDir,
}

/// The `--ensure` deadline the two blocked-startup cases below run under, and
/// the `{Duration:?}` rendering the error carries at that value. One constant
/// so the two can never disagree.
///
/// Opt-in per test, NOT set on every `Fixture::command()`: the rest of this
/// file needs a real coordinator to actually come up, and 1s is not enough for
/// that. Applying it globally failed `invalid_config_reports_startup_failure`
/// and `cancellation_cleans_up_descendants_...` for exactly that reason.
const ENSURE_TIMEOUT_SECS: &str = "1";
const ENSURE_TIMEOUT_RENDERED: &str = "within 1s";

/// Hang guard for fixture events that wait on a spawned helper: `helper.pid`
/// appearing, ensure exiting after cancel or the shipped deadline, a
/// cancelled descendant being reaped.
///
/// The idle path is milliseconds. A starved host has been observed past 5s
/// (phux-rt87) while the same 17 tests pass alone. Matches `bounded_output`:
/// the assertions are cleanup and reap, not speed, so the ceiling only
/// elapses if the helper never starts or ensure never exits. The helper
/// fixtures keep the shipped 10s `--ensure` ceiling — a 3s override raced
/// spawn under load, so the helper never wrote `helper.pid` before the
/// watchdog fired.
const HANG_GUARD: Duration = Duration::from_secs(15);

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("isolated environment");
        let socket = dir.path().join("phux-ensure/phux.sock");
        std::fs::create_dir_all(socket.parent().expect("socket parent")).expect("runtime dir");
        std::fs::create_dir_all(dir.path().join("phux")).expect("config dir");
        Self {
            server: common::AutoSpawnedServer::new(crate::runner::phux_bin(), socket.clone()),
            socket,
            dir,
        }
    }

    fn command(&self) -> Command {
        let mut cmd = common::phux_cmd(crate::runner::phux_bin());
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path())
            .env("XDG_CONFIG_DIRS", self.dir.path())
            .env("XDG_STATE_HOME", self.dir.path())
            .env("XDG_RUNTIME_DIR", self.dir.path())
            .env("PHUX_PROFILE", "ensure")
            .env(common::NO_UPDATE_CHECK_ENV, common::NO_UPDATE_CHECK_VALUE)
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

    /// `phux server --ensure` under the shortened deadline, for the cases
    /// that block startup on purpose and assert the bound fires.
    fn ensure_blocked(&self) -> Output {
        bounded_output(
            self.command()
                .env(phux::ENSURE_TIMEOUT_ENV, ENSURE_TIMEOUT_SECS)
                .args(["server", "--ensure"]),
        )
    }

    fn ensure(&mut self) -> Output {
        let output = bounded_output(self.command().args(["server", "--ensure"]));
        if output.status.success() {
            self.server.capture_pid();
        }
        output
    }

    fn ensure_json(&mut self) -> (Output, serde_json::Value) {
        let output = bounded_output(self.command().args(["server", "--ensure", "--json"]));
        if output.status.success() {
            self.server.capture_pid();
        }
        let document = serde_json::from_slice(&output.stdout).expect("ensure document");
        (output, document)
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
    fixture.config("[defaults]\nsession-name-template = 'bootstrap-seed'\nspawn-on-attach = 'printf seeded > seed-marker; exec /bin/sh'\n");
    assert_success(&fixture.ensure());
    UnixStream::connect(&fixture.socket).expect("accepting immediately after ensure exits");
    let first = fixture.status();
    assert_eq!(first["sessions"][0]["name"], "bootstrap-seed", "{first}");
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
fn json_contract_reports_cold_owner_then_exact_socket_reuse() {
    let mut fixture = Fixture::new();
    let (cold, cold_doc) = fixture.ensure_json();
    assert!(cold.status.success(), "{cold:?}");
    assert!(cold.stderr.is_empty(), "{cold:?}");
    assert_eq!(cold_doc["schema_version"], 1);
    assert_eq!(cold_doc["running"], true);
    assert_eq!(cold_doc["socket"], fixture.socket.display().to_string());
    assert_eq!(cold_doc["disposition"], "daemon_started");
    assert!(cold_doc["cli_version"].is_string());
    assert!(cold_doc["server_log"].is_string());

    let (warm, warm_doc) = fixture.ensure_json();
    assert!(warm.status.success(), "{warm:?}");
    assert!(warm.stderr.is_empty(), "{warm:?}");
    assert_eq!(warm_doc["socket"], cold_doc["socket"]);
    assert_eq!(warm_doc["disposition"], "reused");
    assert_eq!(
        fixture.status()["sessions"].as_array().map(Vec::len),
        Some(1)
    );
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

/// An auto-spawned server writes its stderr into `server.log`, a file: its
/// tracing lines must carry no ANSI color escapes, or `grep` and `phux
/// logs --server` show `^[[2m` noise around every field.
#[test]
fn server_log_from_an_auto_spawned_server_has_no_ansi_escapes() {
    let mut fixture = Fixture::new();
    let output = bounded_output(
        fixture
            .command()
            .env("RUST_LOG", "phux=info")
            .args(["server", "--ensure"]),
    );
    assert_success(&output);
    fixture.server.capture_pid();
    let log = std::fs::read_to_string(fixture.dir.path().join("phux-ensure/server.log"))
        .expect("server log");
    assert!(log.contains("phux server started"), "{log}");
    assert!(
        !log.contains('\u{1b}'),
        "ANSI escapes in the log file:\n{log}"
    );
    fixture.assert_cleaned_up();
}

#[test]
fn foreign_version_coordinator_is_reused_without_reexecution() {
    let mut fixture = Fixture::new();
    assert_success(&fixture.ensure());
    let first = fixture.status();
    let state = fixture.dir.path().join("phux-ensure");
    let history = state.join("server-starts.log");
    // The startup history is the production source of binary-version skew;
    // HELLO negotiates only the protocol. Simulate a separately packaged build
    // while driving an actual coordinator and its real Upgrade/re-exec handler.
    let foreign = format!("1 {} 0.0.0-foreign\n", first["pid"]);
    std::fs::write(&history, &foreign).expect("foreign build history");
    for _ in 0..2 {
        let output = fixture.ensure();
        assert_success(&output);
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("upgrading"),
            "{output:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&history).expect("history"),
            foreign,
            "availability checks must not re-exec a foreign coordinator"
        );
    }
    assert_eq!(fixture.status()["pid"], first["pid"]);
    let log = std::fs::read_to_string(state.join("server.log")).expect("log");
    assert_eq!(log.matches("phux: info: server listening on").count(), 1);
    fixture.assert_cleaned_up();
}

#[test]
fn socket_environment_selects_the_coordinator_without_a_flag() {
    let mut fixture = Fixture::new();
    let profile_socket = fixture.socket.clone();
    fixture.socket = fixture.dir.path().join("environment.sock");
    fixture.server =
        common::AutoSpawnedServer::new(crate::runner::phux_bin(), fixture.socket.clone());
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
fn invalid_config_reports_startup_failure_and_log_path() {
    let mut fixture = Fixture::new();
    fixture.config("[defaults\n");
    let output = fixture.ensure();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The server's own refusal, surfaced from its log rather than a timeout.
    assert!(stderr.contains("exited before accepting"), "{stderr}");
    assert!(stderr.contains("failed to load"), "{stderr}");
    assert!(stderr.contains("server.log"), "{stderr}");
    assert!(output.stdout.is_empty());
    assert!(UnixStream::connect(&fixture.socket).is_err());
}

/// A server that cannot bind exits at once; the wait notices the exit instead
/// of polling out the 2s auto-spawn deadline, and reports the bind error.
#[test]
fn unbindable_socket_fails_fast_with_the_bind_error() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new();
    let locked = fixture.dir.path().join("locked");
    std::fs::create_dir(&locked).expect("locked dir");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    let socket = locked.join("phux.sock");
    let started = Instant::now();
    let output = bounded_output(
        fixture
            .command()
            .args(["server", "--ensure", "--socket"])
            .arg(&socket),
    );
    let elapsed = started.elapsed();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("exited before accepting"), "{stderr}");
    assert!(stderr.contains("failed to bind"), "{stderr}");
    assert!(stderr.contains("server.log"), "{stderr}");
    // Well under the 2s deadline the old poll always ran out.
    assert!(
        elapsed < Duration::from_secs(1),
        "took {elapsed:?}: {stderr}"
    );
    assert!(!socket.exists());
    // The banner prints only once the socket is bound, so a failed bind
    // never logs "listening" ahead of "failed to bind".
    let log = std::fs::read_to_string(fixture.dir.path().join("phux-ensure/server.log"))
        .expect("server log");
    assert!(log.contains("failed to bind"), "{log}");
    assert!(!log.contains("listening"), "{log}");
}

/// A JSON verb that auto-starts the server carries the server's own bind
/// error in its `no_server` document, not just the log path.
#[test]
fn json_new_reports_the_auto_started_servers_bind_error() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new();
    let locked = fixture.dir.path().join("locked");
    std::fs::create_dir(&locked).expect("locked dir");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    let socket = locked.join("phux.sock");
    let output = bounded_output(
        fixture
            .command()
            .args(["new", "--json", "-s", "work", "--socket"])
            .arg(&socket),
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let document: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("one JSON error document on stderr");
    assert_eq!(document["error"]["code"], "no_server", "{document}");
    let auto_start = document["error"]["auto_start_error"]
        .as_str()
        .unwrap_or_else(|| panic!("no auto_start_error: {document}"));
    assert!(auto_start.contains("exited before accepting"), "{document}");
    assert!(auto_start.contains("failed to bind"), "{document}");
    assert!(
        document["remedy"]
            .as_str()
            .is_some_and(|remedy| remedy.contains("auto_start_error")),
        "{document}"
    );
    assert!(!socket.exists());
}

#[test]
fn blocked_config_read_is_bounded_by_the_overall_deadline() {
    let fixture = Fixture::new();
    let status = Command::new("mkfifo")
        .arg(fixture.dir.path().join("phux/config.toml"))
        .status()
        .expect("mkfifo");
    assert!(status.success());
    let started = Instant::now();
    let output = fixture.ensure_blocked();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(started.elapsed() < Duration::from_secs(6));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(ENSURE_TIMEOUT_RENDERED), "{stderr}");
    assert!(!fixture.socket.exists());
}

#[test]
fn blocked_log_open_is_bounded_by_the_overall_deadline() {
    let fixture = Fixture::new();
    let log = fixture.dir.path().join("log-fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&log)
            .status()
            .expect("mkfifo")
            .success()
    );
    let started = Instant::now();
    let output = bounded_output(
        fixture
            .command()
            .env("PHUX_LOG", log)
            .env(phux::ENSURE_TIMEOUT_ENV, ENSURE_TIMEOUT_SECS)
            .args(["server", "--ensure"]),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(started.elapsed() < Duration::from_secs(6));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(ENSURE_TIMEOUT_RENDERED), "{stderr}");
}

mod adoption;
mod descriptors;
