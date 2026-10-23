//! `phux host add HOST` (ADR-0122) where a direct route answers: a real
//! server with a loopback QUIC listener stands in for the far machine, and a
//! fake `ssh` via `$PHUX_SSH` runs the remote half locally with the same
//! state. Hermetic dirs, no overlay detection; `PHUX_WS_SECURE=1` makes the
//! loopback listener take the routable (pinned + token) path.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

// Every process here starts from a scrubbed `PHUX_*` environment: run from
// a phux pane, an inherited `PHUX_WS_TOKENS` / `PHUX_WS_TLS_CERT` would
// point the far host's server and its `phux pair` at the operator's real
// credential store and certificate.
#[path = "../common/ambient.rs"]
mod common;
#[path = "../common/listeners.rs"]
mod listeners;
#[path = "../common/paired.rs"]
mod paired;

mod far_host;

use far_host::{FarHost, stderr};

/// The whole story on one host: `host add` finds phux, pairs, dials the
/// route `ssh -G` named, and registers it direct; the session verbs reach
/// the server through the entry with no ssh; a second `host add` finds the
/// route answering and touches nothing; and after the server is stopped by
/// hand, `phux attach NAME` starts it over ssh and attaches.
#[test]
#[ignore = "spawns a real server and a PTY-backed binary; runs in the e2e lane"]
fn host_add_registers_a_direct_route_and_attach_restarts_a_stopped_server() {
    let mut host = FarHost::start();
    let port = host.port.to_string();

    // 1. Add the machine. The far host's `phux pair --json` is the real one,
    //    against the real listener's token store; the probe dials it.
    let out = host.phux(&["host", "add", "me@box", "--quic-port", &port]);
    let (stdout, err) = (String::from_utf8_lossy(&out.stdout), stderr(&out));
    assert!(
        out.status.success(),
        "host add: stdout={stdout} stderr={err}"
    );
    let endpoint = format!("quic://127.0.0.1:{port}");
    assert!(
        stdout.contains(&format!("Registered box -> {endpoint}"))
            && stdout.contains("phux attach box"),
        "stdout={stdout}\nstderr={err}"
    );
    assert!(
        err.contains("box: paired")
            && err.contains(&format!("direct route reachable at {endpoint}")),
        "stderr={err}"
    );
    let config = host.config();
    assert!(
        config.contains(&format!("endpoint = \"{endpoint}\""))
            && config.contains("ssh = \"me@box\"")
            && !config.contains("direct ="),
        "a route that answered is the endpoint, with nothing left to promote; config={config}"
    );
    assert!(
        host.dir
            .path()
            .join("state/phux/remotes/box.token")
            .exists(),
        "the minted token is stored for the dial"
    );
    let calls = host.ssh_calls();
    assert!(
        calls.contains("phux --version")
            && calls.contains("phux service install")
            && calls.contains("phux pair --json")
            && calls.contains("-G -- me@box"),
        "calls={calls:?}"
    );

    // 2. The entry is enough for the session verbs: no ssh in the path.
    let before = host.ssh_calls().lines().count();
    let out = host.phux(&["ls", "--remote", "box", "--json"]);
    assert!(out.status.success(), "ls --remote box: {}", stderr(&out));
    assert_eq!(
        host.ssh_calls().lines().count(),
        before,
        "a registered direct route never touches ssh"
    );

    // 3. Adding it again finds it answering and re-pairs nothing.
    let out = host.phux(&["host", "add", "me@box", "--quic-port", &port]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "re-add: {}", stderr(&out));
    assert!(
        stdout.contains("already registered and answering"),
        "stdout={stdout}"
    );
    assert_eq!(
        host.ssh_calls().lines().count(),
        before,
        "an answering host is not set up again"
    );

    // 4. Stop the server by hand. The attach finds nobody answering, starts
    //    it over ssh, and attaches with the credentials it already has.
    host.stop_server();
    let seen = host.run_until(&["attach", "box"], "\u{1b}[?1049h");
    assert!(
        seen.contains(&format!("box is not answering at {endpoint}"))
            && seen.contains("starting its server over ssh (me@box)"),
        "the attach must start the stopped server rather than fail; got: {seen}"
    );
    assert!(
        !seen.contains("re-pairing"),
        "the saved credentials still work, so nothing is re-paired; got: {seen}"
    );
    assert!(
        seen.contains("\u{1b}[?1049h"),
        "the TUI came up over the direct route after the restart: {seen}"
    );
    let calls = host.ssh_calls();
    assert_eq!(
        calls.matches("phux pair --json").count(),
        1,
        "pairing happened exactly once, at the add; calls={calls:?}"
    );
}

/// `--install` on a machine without phux: the official installer runs over
/// ssh (on this build's channel), the enrollment then drives the binary it
/// installed by its absolute path, and `--accept-new-host-key` reaches
/// every ssh run. Without `--install` the same host is refused with a
/// remedy that offers it.
#[test]
#[ignore = "spawns a real server; runs in the e2e lane"]
fn host_add_install_puts_phux_on_a_host_without_it() {
    let mut host = FarHost::start_without_phux();
    let port = host.port.to_string();

    let out = host.phux(&["host", "add", "me@box", "--quic-port", &port, "--json"]);
    assert!(!out.status.success(), "no phux there, no --install");
    let err = stderr(&out);
    assert!(
        err.contains("\"code\":\"registry\"") && err.contains("phux host add me@box --install"),
        "the refusal offers --install: {err}"
    );

    let out = host.phux(&[
        "host",
        "add",
        "me@box",
        "--quic-port",
        &port,
        "--install",
        "--accept-new-host-key",
    ]);
    let (stdout, err) = (String::from_utf8_lossy(&out.stdout), stderr(&out));
    assert!(
        out.status.success(),
        "host add --install: stdout={stdout} stderr={err}"
    );
    let installed = host.dir.path().join(".local/bin/phux");
    assert!(
        installed.exists(),
        "the installer linked phux into ~/.local/bin"
    );
    assert!(
        err.contains("box: phux is not installed there; installing the latest")
            && err.contains(&format!("at {}", installed.display()))
            && err.contains("box: paired"),
        "stderr={err}"
    );
    assert!(
        stdout.contains(&format!("Registered box -> quic://127.0.0.1:{port}")),
        "stdout={stdout}\nstderr={err}"
    );
    let calls = host.ssh_calls();
    assert!(
        calls.contains(&format!("{} pair --json", installed.display()))
            && calls.contains(&format!("{} service install", installed.display())),
        "enrollment drives the installed binary by its path: {calls}"
    );
    assert!(
        calls.contains("-o BatchMode=yes -o StrictHostKeyChecking=accept-new me@box sh -c ")
            && calls.matches("-o BatchMode=yes me@box").count() == 1,
        "every remote command after the refused first add trusts a first-seen host key: {calls}"
    );
    let curl = std::fs::read_to_string(host.dir.path().join("curl-calls")).unwrap_or_default();
    assert!(curl.contains("https://phux.sh/install"), "curl={curl}");

    assert!(
        host.config()
            .contains(&format!("remote-phux = \"{}\"", installed.display())),
        "the installed phux is recorded for later ssh commands: {}",
        host.config()
    );

    // With the server stopped, a plain re-add (no --install) reaches it
    // through the recorded phux, which is not on the far PATH.
    host.stop_server();
    let out = host.phux(&["host", "add", "me@box", "--quic-port", &port]);
    assert!(
        out.status.success(),
        "re-add through the recorded phux: {}",
        stderr(&out)
    );

    // Set up again from scratch: the phux installed before is found where
    // the installer put it, not installed a second time.
    let out = host.phux(&["host", "rm", "box"]);
    assert!(out.status.success(), "host rm: {}", stderr(&out));
    let out = host.phux(&["host", "add", "me@box", "--quic-port", &port, "--install"]);
    let err = stderr(&out);
    assert!(out.status.success(), "second host add --install: {err}");
    assert!(!err.contains("installing the latest"), "stderr={err}");
    let curl = std::fs::read_to_string(host.dir.path().join("curl-calls")).unwrap_or_default();
    assert_eq!(curl.lines().count(), 1, "installed once: {curl}");
}
