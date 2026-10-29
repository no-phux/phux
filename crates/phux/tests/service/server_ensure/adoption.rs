//! Service adoption: the init-system helper an ensure runs is bounded,
//! reaped on every exit path, and never takes a live coordinator with it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::*;

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
    let deadline = Instant::now() + HANG_GUARD;
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
    assert!(
        guard.wait_for_exit(HANG_GUARD).is_some(),
        "ensure did not exit"
    );
    assert!(started.elapsed() < HANG_GUARD);
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
    assert!(guard.wait_for_exit(HANG_GUARD).is_some());
    assert!(
        !common::process_exists(helper.0),
        "helper must be reaped before ensure exits"
    );
    // An orphaned grandchild is reaped by init, not by the ensure process.
    let deadline = Instant::now() + HANG_GUARD;
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
        .env("PHUX_BIN", crate::runner::phux_bin())
        .args(["server", "--ensure"])
        .spawn()
        .expect("ensure");
    let mut guard = common::ServerProcess::from_child(child, PathBuf::new());
    let helper = await_helper(&fixture);
    fixture.server.capture_pid();
    common::terminate(guard.child_mut().id());
    assert!(guard.wait_for_exit(HANG_GUARD).is_some());
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
            .env("PHUX_BIN", crate::runner::phux_bin())
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
