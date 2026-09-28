//! `phux service install` must refuse to supervise a socket a live server
//! already holds: the supervised server would fail to bind on every start and
//! the init system would retry forever. `--adopt` is the non-destructive way
//! past the refusal (writes and arms the unit without loading it).
//!
//! Safety: `Manager::unit_path` resolves from `HOME`, not `--socket`, and a
//! real install runs `launchctl bootout`. So `HOME`/`XDG_CONFIG_HOME` point
//! into the tempdir and each refusal test asserts no unit file was written.
//! Never let these tests proceed past the guard and clean up afterwards.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::Path;
use std::process::Command;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// Stop whatever server ended up on `socket`, so a failing assertion cannot
/// leak a daemon holding a PTY (phux-whhd).
struct Cleanup {
    _server: common::AutoSpawnedServer,
    _dir: tempfile::TempDir,
}

/// Every unit path `phux service install` could write under a sandboxed home:
/// both platforms and both profiles (a test binary resolves `dev`), so the
/// absence check can never become a no-op.
fn unit_paths_under(home: &Path) -> [std::path::PathBuf; 4] {
    [
        home.join("Library/LaunchAgents/com.phux.server.plist"),
        home.join(".config/systemd/user/phux.service"),
        home.join("Library/LaunchAgents/com.phux.server.dev.plist"),
        home.join(".config/systemd/user/phux-dev.service"),
    ]
}

/// A `phux` command confined to a sandboxed home: `HOME`, XDG config, and XDG
/// state (where `--adopt` writes its marker) all point into the tempdir, and a
/// nonexistent `PHUX_TAILSCALE` keeps `doctor` off the network.
fn sandboxed(home: &Path) -> Command {
    let mut cmd = Command::new(PHUX);
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env(
            "PHUX_TAILSCALE",
            "/nonexistent/phux-service-install-guard-no-overlay",
        );
    cmd
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
    assert!(common::wait_until_accepting(&socket), "server must be up");
    let mut server = common::AutoSpawnedServer::new(PHUX, socket.clone());
    server.capture_pid();
    let _cleanup = Cleanup {
        _server: server,
        _dir: dir,
    };

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

/// `--print` is a dry run and must keep working whatever is running.
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
    assert!(common::wait_until_accepting(&socket), "server must be up");
    let mut server = common::AutoSpawnedServer::new(PHUX, socket.clone());
    server.capture_pid();
    let _cleanup = Cleanup {
        _server: server,
        _dir: dir,
    };

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

/// `--adopt` (ADR-0088): the unit is written, the incumbent server keeps its
/// socket and panes (no signal, bootout, or `enable --now`), and the output
/// does not claim the running server is now supervised.
#[test]
fn adopt_installs_over_a_live_server_without_stopping_it() {
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
    assert!(common::wait_until_accepting(&socket), "server must be up");
    let mut server = common::AutoSpawnedServer::new(PHUX, socket.clone());
    server.capture_pid();
    let _cleanup = Cleanup {
        _server: server,
        _dir: dir,
    };

    let home = tempfile::tempdir().expect("sandboxed home");
    let install = sandboxed(home.path())
        .args(["service", "install", "--adopt", "--socket"])
        .arg(&socket)
        .output()
        .expect("run phux service install --adopt");

    let stdout = String::from_utf8_lossy(&install.stdout);
    let stderr = String::from_utf8_lossy(&install.stderr);
    assert!(
        install.status.success(),
        "--adopt must succeed over a live server.\nstdout: {stdout}\nstderr: {stderr}"
    );

    // (2) first, because it is the criterion the whole bead exists for: a
    // regression that stops the incumbent must fail here even if everything
    // else about the install is right.
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_ok(),
        "the incumbent server must still be accepting after an adopt install.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );

    // (1) Exactly one unit, and it is the one for this build's platform and
    // profile. Filtering the same list the refusal tests assert *empty* keeps
    // both directions reading against one definition of "a unit was written".
    let written: Vec<_> = unit_paths_under(home.path())
        .into_iter()
        .filter(|path| path.exists())
        .collect();
    assert_eq!(
        written.len(),
        1,
        "--adopt must write exactly one unit, under the sandboxed home; found {written:?}"
    );

    // (3) The banner has to say what did not happen, not just what did.
    assert!(
        stdout.contains("armed"),
        "the adopt banner must say the unit is armed rather than installed.\nstdout: {stdout}"
    );
    assert!(
        stdout.contains("panes"),
        "the adopt banner must account for the running panes.\nstdout: {stdout}"
    );
    assert!(
        !stdout.contains("phux service installed."),
        "an adopt install must not print the ordinary install banner, which reads as \
         'your running server is supervised now'.\nstdout: {stdout}"
    );

    // (4) The armed state is scoped to the socket it was armed against: doctor
    // warns for this socket and not for another from the same home.
    let elsewhere = home.path().join("unrelated.sock");
    let unrelated = sandboxed(home.path())
        .args(["doctor", "--json", "--socket"])
        .arg(&elsewhere)
        .output()
        .expect("run phux doctor --json against an unrelated socket");
    assert!(
        !armed_supervision_reported(&unrelated.stdout),
        "an adoption armed for {} must not be reported while diagnosing {}.\nstdout: {}",
        socket.display(),
        elsewhere.display(),
        String::from_utf8_lossy(&unrelated.stdout)
    );

    let diagnosed = sandboxed(home.path())
        .args(["doctor", "--json", "--socket"])
        .arg(&socket)
        .output()
        .expect("run phux doctor --json against the adopted socket");
    assert!(
        armed_supervision_reported(&diagnosed.stdout),
        "the instance the unit was armed for must still be told that supervision is \
         armed but not active.\nstdout: {}",
        String::from_utf8_lossy(&diagnosed.stdout)
    );
}

/// Does a `phux doctor --json` document carry the armed-supervision warning?
fn armed_supervision_reported(stdout: &[u8]) -> bool {
    let doc: serde_json::Value =
        serde_json::from_slice(stdout).expect("phux doctor --json emits one JSON document");
    doc["checks"]
        .as_array()
        .expect("the document lists checks")
        .iter()
        .any(|check| {
            check["name"] == "server-health"
                && check["detail"]
                    .as_str()
                    .is_some_and(|detail| detail.contains("supervision is armed"))
        })
}
