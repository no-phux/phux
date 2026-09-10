//! The session verbs over `--remote`, end to end (phux-c2td.2).
//!
//! Stands up a real `phux server` with a loopback QUIC listener, registers it
//! as a `[[remote]]` entry, and drives `new`, `ls`, `rename`, `detach`, and
//! `kill` against it from separate processes with `--remote`: the same
//! registry lookup and QUIC dial a host across a network gets, with the
//! network taken out. Loopback QUIC needs neither a token nor a certificate
//! pin (the attach-side trust rule the verbs share), so nothing is minted.
//!
//! Hermetic: private config, state, and runtime dirs; no ssh (a path that
//! does not exist) and no overlay detection, so a cold resolution could only
//! ever fail loudly rather than reach a real host.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// The registry name the loopback listener is registered under.
const REMOTE: &str = "loop";

/// How long the freshly started server has to answer its first `ls`.
const READY_DEADLINE: Duration = Duration::from_secs(30);

/// How long a listing has to reflect a lifecycle change. A killed session
/// leaves the listing once the server has reaped its panes, which happens
/// after the kill is acknowledged, not before.
const SETTLE_DEADLINE: Duration = Duration::from_secs(10);

/// A real server behind a loopback QUIC listener, registered as [`REMOTE`].
struct LoopbackRemote {
    dir: TempDir,
    server: Child,
}

impl LoopbackRemote {
    fn start() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let port = free_udp_port();
        write_registry(dir.path(), port);
        let server = Command::new(PHUX)
            .envs(hermetic_env(dir.path()))
            .arg("server")
            .arg("--socket")
            .arg(dir.path().join("s.sock"))
            .args(["--quic", &format!("127.0.0.1:{port}")])
            // Backstop only: the Drop below kills the server, but a panic
            // that skips it must not leave a daemon behind for long.
            .args(["--exit-after-idle", "120"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server");
        let remote = Self { dir, server };
        remote.await_ready();
        remote
    }

    /// Run `phux <args...>` as a separate client process. It is never handed
    /// the server's socket: `--remote` is the only way it can reach it.
    fn phux(&self, args: &[&str]) -> Output {
        Command::new(PHUX)
            .envs(hermetic_env(self.dir.path()))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run phux")
    }

    fn await_ready(&self) {
        let start = Instant::now();
        loop {
            let out = self.phux(&["ls", "--remote", REMOTE, "--json"]);
            if out.status.success() {
                return;
            }
            assert!(
                start.elapsed() < READY_DEADLINE,
                "the loopback remote never answered `ls --remote`: {}",
                stderr(&out)
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Session names from `phux ls --remote loop --json`.
    fn session_names(&self) -> Vec<String> {
        let out = self.phux(&["ls", "--remote", REMOTE, "--json"]);
        assert!(out.status.success(), "ls --remote: {}", stderr(&out));
        let doc: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("ls --json is a JSON document");
        doc["sessions"]
            .as_array()
            .expect("sessions array")
            .iter()
            .map(|session| session["name"].as_str().expect("name").to_owned())
            .collect()
    }

    /// Wait until every `present` name is listed and no `absent` one is,
    /// failing after [`SETTLE_DEADLINE`]. Polls rather than reading once
    /// because session reaping trails the kill's acknowledgement. The
    /// server's own seed session may also be listed, so this is not equality.
    fn assert_sessions(&self, present: &[&str], absent: &[&str]) {
        let start = Instant::now();
        loop {
            let names = self.session_names();
            let listed = |want: &&str| names.iter().any(|name| name == want);
            if present.iter().all(listed) && !absent.iter().any(listed) {
                return;
            }
            assert!(
                start.elapsed() < SETTLE_DEADLINE,
                "sessions never settled: want {present:?} without {absent:?}, have {names:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for LoopbackRemote {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
    }
}

/// The environment every process in this test runs under.
fn hermetic_env(dir: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("XDG_CONFIG_HOME", dir.join("config")),
        ("XDG_STATE_HOME", dir.join("state")),
        ("XDG_RUNTIME_DIR", dir.join("run")),
        ("PHUX_PROFILE", PathBuf::from("default")),
        // Any ssh or overlay probe fails loudly instead of reaching out.
        ("PHUX_SSH", dir.join("no-such-ssh")),
        ("PHUX_TAILSCALE", dir.join("no-such-tailscale")),
    ]
}

/// Register the loopback listener as [`REMOTE`], the way `phux host add loop
/// quic://127.0.0.1:PORT` would.
fn write_registry(dir: &Path, port: u16) {
    let config_dir = dir.join("config/phux");
    std::fs::create_dir_all(&config_dir).expect("config dir");
    std::fs::create_dir_all(dir.join("run")).expect("runtime dir");
    std::fs::write(
        config_dir.join("config.toml"),
        format!("[[remote]]\nname = \"{REMOTE}\"\nendpoint = \"quic://127.0.0.1:{port}\"\n"),
    )
    .expect("write registry");
}

/// A UDP port nothing is bound to right now. The window between releasing it
/// here and the server binding it is small, and a collision fails loudly at
/// the readiness wait rather than passing by accident.
fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .expect("bind probe socket")
        .local_addr()
        .expect("probe addr")
        .port()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Create, list, rename, detach, and kill sessions on a remote from a local
/// shell — the gap F15/F19 named: headless verbs that took `--socket` only.
#[test]
#[ignore = "spawns a real server with a QUIC listener; runs in the e2e lane"]
fn session_verbs_create_list_rename_and_kill_over_remote() {
    let remote = LoopbackRemote::start();

    for name in ["keep", "build"] {
        let out = remote.phux(&["new", "--remote", REMOTE, "-s", name, "--json"]);
        assert!(out.status.success(), "new {name}: {}", stderr(&out));
        let doc: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("new --json is a JSON document");
        assert_eq!(doc["session"], name);
        assert!(
            doc["terminal_id"].as_u64().is_some(),
            "the seed pane id comes back over the remote dial: {doc}"
        );
    }
    remote.assert_sessions(&["keep", "build"], &[]);

    // A duplicate is refused against the remote's own snapshot.
    let out = remote.phux(&["new", "--remote", REMOTE, "-s", "keep", "--json"]);
    assert!(!out.status.success(), "a duplicate name must be refused");

    let out = remote.phux(&["rename", "--remote", REMOTE, "build", "ci"]);
    assert!(out.status.success(), "rename: {}", stderr(&out));
    assert!(stdout(&out).contains("renamed"), "{}", stdout(&out));
    remote.assert_sessions(&["keep", "ci"], &["build"]);

    let out = remote.phux(&["detach", "--remote", REMOTE, "keep"]);
    assert!(out.status.success(), "detach: {}", stderr(&out));
    assert!(
        stdout(&out).contains("detached 0 client(s)"),
        "{}",
        stdout(&out)
    );

    let out = remote.phux(&["kill", "--remote", REMOTE, "ci"]);
    assert!(out.status.success(), "kill: {}", stderr(&out));
    remote.assert_sessions(&["keep"], &["ci", "build"]);

    // The human listing rides the same dial.
    let out = remote.phux(&["ls", "--remote", REMOTE]);
    assert!(out.status.success(), "ls: {}", stderr(&out));
    assert!(stdout(&out).contains("keep:"), "{}", stdout(&out));
}
