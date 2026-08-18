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
//! and say so. `--adopt` (ADR-0088, phux-m3ot) is the way past the refusal
//! that costs neither: it writes the unit and arms it rather than loading it,
//! so nothing binds twice and nothing is stopped. Its test lives here too,
//! because it is the same guard viewed from the other side.
//!
//! These tests drive the REAL compiled binary against a REAL socket. In the
//! refusal tests nothing is installed: the refusal is asserted to happen
//! *before* any unit is written, which is the whole point — an install that
//! fails after writing would leave the loop behind.
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
//!
//! The `--adopt` test is the one that does write a unit, and it is safe for
//! the same two reasons inverted: it writes into the sandboxed `HOME`, and
//! `--adopt` never runs `bootout`, `bootstrap`, or `enable --now`. The only
//! init-system call it can make is `systemctl --user enable` *without*
//! `--now`, against a unit search path that `XDG_CONFIG_HOME` has already
//! redirected into the tempdir.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

mod common;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
const DEADLINE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

/// Stop whatever server ended up on `socket`, so a failing assertion cannot
/// leak a daemon holding a PTY (phux-whhd).
struct Cleanup {
    _server: common::AutoSpawnedServer,
    _dir: tempfile::TempDir,
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
///
/// `XDG_STATE_HOME` is redirected for the same reason as the other two, and it
/// is load-bearing for `--adopt`: that path writes an adoption marker into the
/// state directory, and a marker in the *developer's real* state directory
/// would make their next cold `phux` try to bootstrap a service unit
/// (ADR-0088). Sandboxing the state dir keeps the marker inside the tempdir
/// that is deleted with the test.
fn sandboxed(home: &Path) -> Command {
    let mut cmd = Command::new(PHUX);
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_STATE_HOME", home.join(".local/state"));
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
    assert!(wait_until_accepting(&socket), "server must be up");
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
    assert!(wait_until_accepting(&socket), "server must be up");
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

/// The way past the refusal that costs neither the panes nor a crash-loop
/// (phux-m3ot, ADR-0088).
///
/// Three properties, and all three are the point:
///
///   1. **The unit is written.** Unlike every other test here, `--adopt`
///      commits the supervision — so this asserts the file exists rather than
///      that it does not.
///   2. **The incumbent is untouched.** No signal, no `bootout`, no
///      `enable --now`; the server that held the socket before the install
///      still answers on it afterwards, with its panes.
///   3. **The output does not claim more than it did.** An adopt install that
///      printed the ordinary "phux service installed." banner would leave the
///      user believing their running server is now restart-supervised, which
///      is the one wrong belief this path exists to prevent — no supervisor
///      can adopt a pid it did not start.
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
    assert!(wait_until_accepting(&socket), "server must be up");
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

    // (4) The armed state is scoped to the socket the unit was armed against.
    //
    // `phux doctor` reports armed supervision (ADR-0088, phux-8514), and its
    // reader had dropped the socket guard `complete_pending_adoption` keeps —
    // so a marker armed for *this* socket would have warned on every other
    // instance diagnosed from the same home, describing a server that is not
    // the one the rest of the run is about. Both directions are asserted,
    // because a guard that says "no" to everything would also pass the first.
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
