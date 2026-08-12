//! Binary-level regression for phux-67wg: `phux service install` must refuse
//! to supervise a socket a live server already holds.
//!
//! The bug had no coverage at all. `service.rs`'s test module pins unit
//! *rendering* thoroughly — seventeen tests — but `run_install` itself, and
//! therefore every precondition it does or does not check, was untested.
//!
//! What went wrong: install wrote the unit and handed it to the init system
//! without looking at the socket. The supervised server then binds the same
//! path, `handle_existing_socket` refuses with `SocketBusy` before `bind(2)`
//! is reached, and the process exits non-zero — deterministically, every
//! start. Under the ADR-0080 restart policy that is not a one-off failure but
//! a permanent loop: launchd's `ThrottleInterval` is a minimum spacing rather
//! than a give-up count, and the systemd unit set no `StartLimitBurst`, so
//! neither platform ever stopped retrying. One failed start every 30s, for as
//! long as the incumbent server lives.
//!
//! Stopping the incumbent instead would be worse — it owns live panes and
//! their in-flight shells and agents — so the correct behaviour is to refuse
//! and say so. Moving a running server under supervision without losing its
//! panes is phux-m3ot, and needs mechanism phux does not have yet.
//!
//! These tests drive the REAL compiled binary against a REAL socket. Nothing
//! is installed: the refusal is asserted to happen *before* any unit is
//! written, which is the whole point — an install that fails after writing
//! would leave the loop behind.
//!
//! # Do not let these tests reach a real install
//!
//! `Manager::unit_path` resolves from `HOME`, **not** from `--socket`. A
//! `phux service install` that gets past the guard therefore writes to the
//! developer's own `~/Library/LaunchAgents/com.phux.server.plist` (or
//! `$XDG_CONFIG_HOME/systemd/user/phux.service`) and then runs `launchctl
//! bootout gui/$UID/com.phux.server` — which would tear down whatever real
//! phux service that machine is running, panes and all.
//!
//! Two things keep that from happening, and both must stay:
//!
//!   1. `HOME` and `XDG_CONFIG_HOME` are redirected into the test's own
//!      tempdir, so a regression writes there rather than into a real home.
//!   2. Each test asserts **no unit file was created**, which is what proves
//!      the guard runs before `unit_path()` rather than after it.
//!
//! Do not "simplify" this by letting the install proceed and cleaning up
//! afterwards. There is no cleanup for a `launchctl bootout` that killed
//! someone's panes.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
const DEADLINE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

/// Stop whatever server ended up on `socket`, so a failing assertion cannot
/// leak a daemon holding a PTY (phux-whhd).
struct Cleanup {
    socket: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let Ok(out) = Command::new(PHUX)
            .args(["status", "--json", "--socket"])
            .arg(&self.socket)
            .output()
        else {
            return;
        };
        let Ok(doc) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
            return;
        };
        let Some(pid) = doc["pid"].as_i64() else {
            return;
        };
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output();
        // Wait for the exit before the `TempDir` field drops out from under
        // the dying server.
        let deadline = Instant::now() + DEADLINE;
        while Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(&self.socket).is_err() {
                return;
            }
            std::thread::sleep(POLL);
        }
    }
}

/// Every unit path `phux service install` could write, under a sandboxed home.
///
/// Both platforms' paths are checked regardless of which one this build
/// targets, so the assertion does not silently become a no-op on the other.
///
/// Both *profiles* are checked for the same reason (phux-gyza): the unit name
/// is scoped by the ADR-0080 profile, and a test binary resolves to `dev`, so
/// checking only the default-profile names would let a regression write the
/// file this suite exists to prove is never written.
fn unit_paths_under(home: &Path) -> [std::path::PathBuf; 4] {
    [
        home.join("Library/LaunchAgents/com.phux.server.plist"),
        home.join(".config/systemd/user/phux.service"),
        home.join("Library/LaunchAgents/com.phux.server.dev.plist"),
        home.join(".config/systemd/user/phux-dev.service"),
    ]
}

/// Redirect a child `phux` at a sandboxed home, so nothing it writes can
/// land in the developer's real one. See this module's header.
fn sandboxed(home: &Path) -> Command {
    let mut cmd = Command::new(PHUX);
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"));
    cmd
}

fn wait_until_accepting(socket: &Path) -> bool {
    let deadline = Instant::now() + DEADLINE;
    while Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return true;
        }
        std::thread::sleep(POLL);
    }
    false
}

/// The regression: install against a live socket must fail, and must say why.
#[test]
fn install_refuses_while_a_server_holds_the_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");

    let out = Command::new(PHUX)
        .args(["new", "--session", "incumbent", "--json", "--socket"])
        .arg(&socket)
        .output()
        .expect("run phux new");
    assert!(
        out.status.success(),
        "phux new must start a server.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _cleanup = Cleanup {
        socket: socket.clone(),
        _dir: dir,
    };
    assert!(wait_until_accepting(&socket), "server must be up");

    let home = tempfile::tempdir().expect("sandboxed home");
    let install = sandboxed(home.path())
        .args(["service", "install", "--socket"])
        .arg(&socket)
        .output()
        .expect("run phux service install");

    assert!(
        !install.status.success(),
        "installing over a live server must fail rather than write a unit that \
         cannot bind.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );

    let stderr = String::from_utf8_lossy(&install.stderr);
    assert!(
        stderr.contains("already running"),
        "the refusal must name the real cause, not a generic failure.\nstderr: {stderr}"
    );
    assert!(
        stderr.contains(&socket.display().to_string()),
        "the refusal must name the socket the user has to free.\nstderr: {stderr}"
    );

    // The load-bearing half: refusing *after* writing the unit would leave the
    // restart loop installed, which is the bug. This is also what keeps the
    // test safe to run on a machine with a real phux service (see the header).
    for unit in unit_paths_under(home.path()) {
        assert!(
            !unit.exists(),
            "the guard must run before any unit is written; found {}",
            unit.display()
        );
    }
}

/// The direction the fix could over-correct in: a `--print` dry run touches
/// nothing and must keep working regardless of what is running.
///
/// Without this, "refuse when a server is live" could reasonably be
/// implemented one step too early and break the one subcommand whose entire
/// purpose is to be safe to run at any time.
#[test]
fn print_still_renders_while_a_server_holds_the_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");

    let out = Command::new(PHUX)
        .args(["new", "--session", "incumbent", "--json", "--socket"])
        .arg(&socket)
        .output()
        .expect("run phux new");
    assert!(out.status.success(), "phux new must start a server");
    let _cleanup = Cleanup {
        socket: socket.clone(),
        _dir: dir,
    };
    assert!(wait_until_accepting(&socket), "server must be up");

    let home = tempfile::tempdir().expect("sandboxed home");
    let printed = sandboxed(home.path())
        .args(["service", "install", "--print", "--socket"])
        .arg(&socket)
        .output()
        .expect("run phux service install --print");

    assert!(
        printed.status.success(),
        "--print is a dry run and must not be gated on the socket.\nstderr: {}",
        String::from_utf8_lossy(&printed.stderr)
    );
    let stdout = String::from_utf8_lossy(&printed.stdout);
    assert!(
        stdout.contains("phux"),
        "--print must still render the unit.\nstdout: {stdout}"
    );
    for unit in unit_paths_under(home.path()) {
        assert!(
            !unit.exists(),
            "a dry run must touch nothing; found {}",
            unit.display()
        );
    }
}
