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

/// The deadline the adoption-helper case runs under. Longer than
/// [`ENSURE_TIMEOUT_SECS`] on purpose: that test needs the fake init tool to
/// start and write `helper.pid` BEFORE the deadline fires, so a 1s bound would
/// race the thing the test is trying to observe.
const HELPER_TIMEOUT_SECS: &str = "3";

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
    assert_eq!(log.matches("phux server listening on").count(), 1);
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

/// Real adoption inputs, but every init-system executable is a private fake.
fn arm_fake_service(fixture: &Fixture, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let unit = if cfg!(target_os = "macos") {
        fixture
            .dir
            .path()
            .join("Library/LaunchAgents/com.phux.server.ensure.plist")
    } else {
        fixture.dir.path().join("systemd/user/phux-ensure.service")
    };
    std::fs::create_dir_all(unit.parent().expect("unit parent")).expect("unit dir");
    // No socket override: the isolated ensure profile supplies the default.
    std::fs::write(unit, "").expect("unit");
    let state = fixture.dir.path().join("phux-ensure");
    std::fs::write(state.join("service-adopt-pending"), "armed\n").expect("marker");
    let bin = fixture.dir.path().join("bin");
    std::fs::create_dir(&bin).expect("bin");
    for name in ["launchctl", "systemctl"] {
        let tool = bin.join(name);
        std::fs::write(&tool, script).expect("fake tool");
        std::fs::set_permissions(tool, std::fs::Permissions::from_mode(0o755)).expect("executable");
    }
    bin
}

struct HelperPid(u32);

impl Drop for HelperPid {
    fn drop(&mut self) {
        if common::process_exists(self.0) {
            let pid = rustix::process::Pid::from_raw(i32::try_from(self.0).expect("pid"))
                .expect("nonzero");
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
}

fn await_helper(fixture: &Fixture) -> HelperPid {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = std::fs::read_to_string(fixture.dir.path().join("helper.pid"))
            && let Ok(pid) = text.trim().parse()
        {
            return HelperPid(pid);
        }
        assert!(Instant::now() < deadline, "adoption helper did not start");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn assert_helper_cleaned_up(signal: Option<rustix::process::Signal>, script: &str) {
    let fixture = Fixture::new();
    let bin = arm_fake_service(&fixture, script);
    let child = fixture
        .command()
        .env("PATH", bin)
        .env(phux::ENSURE_TIMEOUT_ENV, HELPER_TIMEOUT_SECS)
        .args(["server", "--ensure"])
        .spawn()
        .expect("ensure");
    let mut guard = common::ServerProcess::from_child(child, PathBuf::new());
    let helper = await_helper(&fixture);
    let started = Instant::now();
    if let Some(signal) = signal {
        let pid =
            rustix::process::Pid::from_raw(i32::try_from(guard.child_mut().id()).expect("pid"))
                .expect("nonzero");
        rustix::process::kill_process(pid, signal).expect("cancel ensure");
    }
    let limit = if signal.is_some() {
        Duration::from_secs(3)
    } else {
        // The shortened deadline above plus slack for a loaded pool.
        Duration::from_secs(6)
    };
    assert!(guard.wait_for_exit(limit).is_some(), "ensure did not exit");
    assert!(started.elapsed() < limit);
    assert_eq!(
        guard
            .child_mut()
            .try_wait()
            .expect("wait")
            .expect("exited")
            .code(),
        Some(1)
    );
    assert!(
        !common::process_exists(helper.0),
        "ensure orphaned temporary helper {}",
        helper.0
    );
    assert!(
        !fixture.socket.exists(),
        "cancelled adoption must not auto-spawn"
    );
}

#[test]
fn deadline_kills_and_reaps_the_adoption_helper() {
    assert_helper_cleaned_up(
        None,
        "#!/bin/sh\necho $$ > helper.pid\nexec /bin/sleep 60\n",
    );
}

#[test]
fn sigterm_kills_and_reaps_the_adoption_helper() {
    assert_helper_cleaned_up(
        Some(rustix::process::Signal::TERM),
        "#!/bin/sh\necho $$ > helper.pid\nexec /bin/sleep 60\n",
    );
}

#[test]
fn sigint_reaps_a_helper_that_closed_its_output_before_exiting() {
    // Exercise cancellation during waitid polling, rather than a pipe read.
    assert_helper_cleaned_up(
        Some(rustix::process::Signal::INT),
        "#!/bin/sh\nexec 2>/dev/null\necho $$ > helper.pid\nexec /bin/sleep 60\n",
    );
}

#[test]
fn cancellation_cleans_up_descendants_even_after_the_helper_leader_exits() {
    let fixture = Fixture::new();
    // The background descendant holds the captured stderr pipe open after the
    // shell exits. Cleanup must retain ownership of the leader until group kill.
    let bin = arm_fake_service(
        &fixture,
        "#!/bin/sh\n/bin/sleep 60 &\necho $! > descendant.pid\necho $$ > helper.pid\nexit 0\n",
    );
    let child = fixture
        .command()
        .env("PATH", bin)
        .args(["server", "--ensure"])
        .spawn()
        .expect("ensure");
    let mut guard = common::ServerProcess::from_child(child, PathBuf::new());
    let helper = await_helper(&fixture);
    let descendant = HelperPid(
        std::fs::read_to_string(fixture.dir.path().join("descendant.pid"))
            .expect("descendant pid")
            .trim()
            .parse()
            .expect("pid"),
    );
    common::terminate(guard.child_mut().id());
    assert!(guard.wait_for_exit(Duration::from_secs(3)).is_some());
    assert!(
        !common::process_exists(helper.0),
        "helper must be reaped before ensure exits"
    );
    // An orphaned grandchild is reaped by init, not by the ensure process.
    let deadline = Instant::now() + Duration::from_secs(3);
    while common::process_exists(descendant.0) {
        assert!(
            Instant::now() < deadline,
            "temporary descendant survived cancellation"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn cancellation_preserves_a_coordinator_that_has_already_detached() {
    let mut fixture = Fixture::new();
    let bin = arm_fake_service(
        &fixture,
        "#!/bin/sh\n\"$PHUX_BIN\" server --daemonize --session adopted --exit-after-idle 30 </dev/null >/dev/null 2>&1 &\necho $$ > helper.pid\nexec /bin/sleep 60\n",
    );
    let child = fixture
        .command()
        .env("PATH", bin)
        .env("PHUX_BIN", PHUX)
        .args(["server", "--ensure"])
        .spawn()
        .expect("ensure");
    let mut guard = common::ServerProcess::from_child(child, PathBuf::new());
    let helper = await_helper(&fixture);
    fixture.server.capture_pid();
    common::terminate(guard.child_mut().id());
    assert!(guard.wait_for_exit(Duration::from_secs(3)).is_some());
    assert!(!common::process_exists(helper.0));
    UnixStream::connect(&fixture.socket).expect("detached coordinator survives cancellation");
    assert_eq!(fixture.status()["sessions"][0]["name"], "adopted");
    fixture.assert_cleaned_up();
}

#[test]
fn successful_service_helper_is_reaped_and_coordinator_survives() {
    let mut fixture = Fixture::new();
    let bin = arm_fake_service(
        &fixture,
        "#!/bin/sh\n\"$PHUX_BIN\" server --daemonize --session adopted --exit-after-idle 30 </dev/null >/dev/null 2>&1 &\necho $$ > helper.pid\nwhile ! \"$PHUX_BIN\" status --json >/dev/null 2>&1; do /bin/sleep 0.025; done\nexit 0\n",
    );
    let output = bounded_output(
        fixture
            .command()
            .env("PATH", bin)
            .env("PHUX_BIN", PHUX)
            .args(["server", "--ensure"]),
    );
    let helper = await_helper(&fixture);
    assert_success(&output);
    fixture.server.capture_pid();
    assert!(!common::process_exists(helper.0));
    assert_eq!(fixture.status()["sessions"][0]["name"], "adopted");
    fixture.assert_cleaned_up();
}

#[test]
fn live_ensure_sweeps_a_stale_adoption_marker() {
    let mut fixture = Fixture::new();
    assert_success(&fixture.ensure());
    let marker = fixture.dir.path().join("phux-ensure/service-adopt-pending");
    std::fs::write(&marker, "stale\n").expect("stale marker");
    assert_success(&fixture.ensure());
    assert!(
        !marker.exists(),
        "ensure_server's live path must sweep a marker whose unit has vanished (phux-dqf3)"
    );
    fixture.assert_cleaned_up();
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
