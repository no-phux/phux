//! `phux service reconcile` must correct an installed unit's restart policy
//! without stopping the server: the real binary against a real server, which
//! must still accept connections afterwards. `HOME`/`XDG_CONFIG_HOME` point
//! into the tempdir and `PHUX_PROFILE` is pinned, so no real unit is touched.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// The profile these tests pin, so the unit path is a fact rather than a
/// consequence of where cargo put the binary.
const PROFILE: &str = "dev";

/// Stop whatever server ended up on `socket`, so a failing assertion cannot
/// leak a daemon holding a PTY (phux-whhd).
struct Cleanup {
    _server: common::AutoSpawnedServer,
    _dir: tempfile::TempDir,
}

/// Where this host's `phux service install` would have written its unit,
/// under a sandboxed home.
fn unit_path_under(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/LaunchAgents")
            .join(format!("com.phux.server.{PROFILE}.plist"))
    } else {
        home.join(".config/systemd/user")
            .join(format!("phux-{PROFILE}.service"))
    }
}

/// A unit in the shape `phux service install` wrote before phux-zomb.4:
/// restart on every exit, unthrottled, with a socket override and `--hub`
/// baked in — the flags a reinstall would silently drop.
fn legacy_unit(socket: &Path) -> String {
    let socket = socket.display();
    if cfg!(target_os = "macos") {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
             \x20 <key>Label</key>\n\
             \x20 <string>com.phux.server.{PROFILE}</string>\n\
             \x20 <key>ProgramArguments</key>\n\
             \x20 <array>\n\
             \x20   <string>/usr/local/bin/phux</string>\n\
             \x20   <string>server</string>\n\
             \x20   <string>--hub</string>\n\
             \x20 </array>\n\
             \x20 <key>RunAtLoad</key>\n\
             \x20 <true/>\n\
             \x20 <key>KeepAlive</key>\n\
             \x20 <true/>\n\
             \x20 <key>EnvironmentVariables</key>\n\
             \x20 <dict>\n\
             \x20   <key>PHUX_SOCKET</key>\n\
             \x20   <string>{socket}</string>\n\
             \x20 </dict>\n\
             </dict>\n\
             </plist>\n"
        )
    } else {
        format!(
            "[Unit]\n\
             Description=phux terminal control plane server\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart=/usr/local/bin/phux server --hub\n\
             Restart=always\n\
             Environment=\"PHUX_SOCKET={socket}\"\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n"
        )
    }
}

/// Redirect a child `phux` at a sandboxed home and a pinned profile, so
/// nothing it writes can land in the developer's real one.
fn sandboxed(home: &Path) -> Command {
    let mut cmd = common::phux_cmd(crate::runner::phux_bin());
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("PHUX_PROFILE", PROFILE);
    cmd
}

/// Plant `legacy_unit` at the path this host's reconcile will look at.
fn plant_legacy_unit(home: &Path, socket: &Path) -> PathBuf {
    let unit = unit_path_under(home);
    std::fs::create_dir_all(unit.parent().expect("unit path has a parent")).expect("mkdir");
    std::fs::write(&unit, legacy_unit(socket)).expect("write legacy unit");
    unit
}

/// Start a real server on a temp socket and return the guard that stops it.
fn live_server() -> (PathBuf, Cleanup) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    let out = common::phux_cmd(crate::runner::phux_bin())
        .args(["new", "--session", "incumbent", "--json", "--socket"])
        .arg(&socket)
        .output()
        .expect("run phux new");
    assert!(
        out.status.success(),
        "phux new must start a server.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut server = common::AutoSpawnedServer::new(crate::runner::phux_bin(), socket.clone());
    server.capture_pid();
    let cleanup = Cleanup {
        _server: server,
        _dir: dir,
    };
    assert!(common::wait_until_accepting(&socket), "server must be up");
    (socket, cleanup)
}

/// The policy is corrected, the operator's flags (`--hub`, `--socket`)
/// survive, and the running server is still running.
#[test]
fn reconcile_corrects_the_policy_without_stopping_the_server() {
    let (socket, _cleanup) = live_server();
    let home = tempfile::tempdir().expect("sandboxed home");
    let unit = plant_legacy_unit(home.path(), &socket);

    let out = sandboxed(home.path())
        .args(["service", "reconcile"])
        .output()
        .expect("run phux service reconcile");
    assert!(
        out.status.success(),
        "reconcile must succeed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let body = std::fs::read_to_string(&unit).expect("read reconciled unit");
    if cfg!(target_os = "macos") {
        assert!(
            body.contains("<key>SuccessfulExit</key>")
                && body.contains("<key>ThrottleInterval</key>"),
            "the corrected policy is missing:\n{body}"
        );
        assert!(
            !body.contains("<key>KeepAlive</key>\n  <true/>"),
            "the unconditional KeepAlive survived:\n{body}"
        );
        assert!(
            body.contains("<string>--hub</string>"),
            "the operator's --hub was dropped:\n{body}"
        );
    } else {
        assert!(
            body.contains("Restart=on-failure") && body.contains("RestartSec="),
            "the corrected policy is missing:\n{body}"
        );
        assert!(
            !body.contains("Restart=always"),
            "the restart-on-any-exit policy survived:\n{body}"
        );
        assert!(
            body.contains("ExecStart=/usr/local/bin/phux server --hub"),
            "the operator's --hub was dropped:\n{body}"
        );
    }
    assert!(
        body.contains(&socket.display().to_string()),
        "the unit's socket override was dropped:\n{body}"
    );

    // The load-bearing assertion. A reconcile that reloaded the unit would
    // have SIGTERMed this server, and the session created above — plus every
    // process inside it — would be gone.
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_ok(),
        "reconcile stopped the running server; that is the bug this verb exists to avoid"
    );
}

/// The output says whether the new policy is in force: systemd re-reads the
/// unit live; launchd keeps the bootstrapped policy until the job restarts.
#[test]
fn reconcile_says_whether_the_new_policy_is_actually_in_force() {
    let (socket, _cleanup) = live_server();
    let home = tempfile::tempdir().expect("sandboxed home");
    plant_legacy_unit(home.path(), &socket);

    let out = sandboxed(home.path())
        .args(["service", "reconcile"])
        .output()
        .expect("run phux service reconcile");
    let stdout = String::from_utf8_lossy(&out.stdout);

    if cfg!(target_os = "macos") {
        assert!(
            stdout.contains("NOT active yet"),
            "macOS must not imply the loaded job picked the policy up.\nstdout: {stdout}"
        );
        assert!(
            stdout.contains("next login or reboot"),
            "the user needs to know when it does take effect.\nstdout: {stdout}"
        );
        assert!(
            stdout.contains("launchctl bootout"),
            "the escape hatch must be an exact command, not a description.\nstdout: {stdout}"
        );
        assert!(
            stdout.contains("phux ls"),
            "a live server means panes are at stake; say how to see them.\nstdout: {stdout}"
        );
    } else {
        assert!(
            stdout.contains("daemon-reload") || stdout.contains("re-read the unit"),
            "Linux must say the reload happened and cost nothing.\nstdout: {stdout}"
        );
    }
    assert!(
        stdout.contains("nothing was stopped") || stdout.contains("untouched"),
        "the headline promise of the verb must be in its output.\nstdout: {stdout}"
    );
}

/// `--print` renders what would land and writes nothing.
#[test]
fn print_renders_the_reconciled_unit_without_writing_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("sandboxed home");
    let unit = plant_legacy_unit(home.path(), &dir.path().join("phux.sock"));
    let before = std::fs::read_to_string(&unit).expect("read planted unit");

    let out = sandboxed(home.path())
        .args(["service", "reconcile", "--print"])
        .output()
        .expect("run phux service reconcile --print");
    assert!(
        out.status.success(),
        "a dry run must succeed.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let corrected = if cfg!(target_os = "macos") {
        "<key>SuccessfulExit</key>"
    } else {
        "Restart=on-failure"
    };
    assert!(
        stdout.contains(corrected),
        "--print must render the reconciled unit.\nstdout: {stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(&unit).expect("re-read planted unit"),
        before,
        "a dry run must not touch the installed unit"
    );
}

/// `install` over a live server with a legacy unit points at the
/// non-destructive `reconcile` instead of "stop the server".
#[test]
fn install_over_a_live_server_points_at_the_non_destructive_path() {
    let (socket, _cleanup) = live_server();
    let home = tempfile::tempdir().expect("sandboxed home");
    let unit = plant_legacy_unit(home.path(), &socket);
    let before = std::fs::read_to_string(&unit).expect("read planted unit");

    let out = sandboxed(home.path())
        .args(["service", "install", "--socket"])
        .arg(&socket)
        .output()
        .expect("run phux service install");
    assert!(
        !out.status.success(),
        "installing over a live server must still fail.\nstdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("phux service reconcile"),
        "the refusal must name the remedy that costs nothing.\nstderr: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&unit).expect("re-read planted unit"),
        before,
        "a refused install must not have touched the unit"
    );
}

/// With no unit installed, reconcile fails rather than claiming success.
#[test]
fn reconcile_reports_a_missing_unit_instead_of_claiming_success() {
    let home = tempfile::tempdir().expect("sandboxed home");
    let out = sandboxed(home.path())
        .args(["service", "reconcile"])
        .output()
        .expect("run phux service reconcile");

    assert!(
        !out.status.success(),
        "nothing was reconciled, so this must not exit 0.\nstdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("not installed"),
        "the report must name the real state.\nstdout: {stdout}"
    );
    assert!(
        stdout.contains("phux service install"),
        "and point at the command that would create one.\nstdout: {stdout}"
    );
}
