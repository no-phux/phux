//! Binary-level acceptance drill for graceful server upgrade (ADR-0032,
//! phux-fak5): a real `phux server` child, a real PTY-backed pane, a real
//! in-place `execve`, and the assertion that the headline promise holds —
//! **the pane's child process and its scrollback survive a binary update, and
//! the resumed pane retains its normal kill/reap lifecycle.**
//!
//! The decisive signal is that the server's PID is *unchanged* after the
//! upgrade: a kill-and-restart would replace it, but a graceful `execve`
//! preserves it in place (and with it the open PTY masters that keep the
//! children alive). We additionally check the pane child is still alive and
//! the on-screen marker survived the snapshot replay.
//!
//! Like the other binary e2e tests it is `#[ignore]` — it spawns a real
//! server and re-execs it, so it starves in the full parallel pool. Run via
//! `just e2e` (or `cargo test -p phux --test upgrade_e2e -- --ignored`).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
const SESSION: &str = "work";
const SOCKET_DEADLINE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

/// Idle lifetime for the harness server, as a backstop UNDER the `Drop` kill
/// (ADR-0063). The guard is still the primary cleanup; it cannot run if the
/// test process is `SIGKILL`ed, and what leaks then is a daemon holding a live
/// PTY forever. Ten minutes is far longer than any gap between this file's
/// client connections, so it can only fire after the harness is gone.
///
/// It carries a second signal here for free: the re-exec argv is rebuilt by
/// the server itself from `RuntimeFlags`, so if an upgrade ever dropped the
/// lifetime, the resumed image in this very test would be the immortal one.
const SERVER_IDLE_LIMIT_SECS: &str = "600";

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct ServerGuard {
    child: Child,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl ServerGuard {
    /// Spawn `phux server` with a seed pane running `seed_command`, then block
    /// until the socket appears.
    fn start_with_seed(seed_command: &str) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = dir
            .path()
            .join(format!("upg-{}-{n}.sock", std::process::id()));
        let child = Command::new(PHUX)
            .args([
                "server",
                "--session",
                SESSION,
                "--seed-command",
                seed_command,
                "--socket",
            ])
            .arg(&socket)
            .args(["--exit-after-idle", SERVER_IDLE_LIMIT_SECS])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server");
        let guard = Self {
            child,
            socket,
            _dir: dir,
        };
        guard.wait_for_socket();
        guard
    }

    fn wait_for_socket(&self) {
        let deadline = Instant::now() + SOCKET_DEADLINE;
        while Instant::now() < deadline {
            if self.socket.exists() {
                return;
            }
            std::thread::sleep(POLL);
        }
        panic!("server did not bind {} in time", self.socket.display());
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let (verb, rest) = args.split_first().expect("a verb");
        let mut c = Command::new(PHUX);
        c.arg(verb)
            .arg("--socket")
            .arg(&self.socket)
            .args(rest)
            .stdin(Stdio::null());
        c
    }

    fn status(&self, args: &[&str]) -> i32 {
        self.cmd(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run verb")
            .code()
            .expect("exited normally")
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self
            .cmd(args)
            .stderr(Stdio::null())
            .output()
            .expect("verb output");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// `pid` is alive iff `kill -0 pid` succeeds (POSIX; no `libc` dependency,
/// which is macOS-gated in the server crate anyway).
fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The first direct child PID of `parent`, via `pgrep -P`.
fn child_of(parent: u32) -> Option<u32> {
    let out = Command::new("pgrep")
        .args(["-P", &parent.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|l| l.trim().parse().ok())
}

fn poll<F: FnMut() -> bool>(deadline: Duration, mut f: F) -> bool {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(POLL);
    }
    false
}

#[test]
#[ignore = "spawns a real phux server + performs a real in-place re-exec; run via `just e2e`."]
fn child_and_scrollback_survive_graceful_upgrade() {
    let marker = format!("UPGRADE_SURVIVES_{}", std::process::id());
    // The seed pane prints the marker, then `exec`s a long-lived `sleep` — so
    // the pane's child is a stable, observable process (the shell's pid is
    // preserved across its own exec).
    let server = ServerGuard::start_with_seed(&format!("printf '{marker}\\n'; exec sleep 600"));
    let server_pid = server.child.id();

    // The pane child (the sleep) must come up, and the marker must reach the
    // grid (observable via `wait --until`).
    let child_pid = {
        let mut found = None;
        assert!(
            poll(Duration::from_secs(10), || {
                found = child_of(server_pid);
                found.is_some()
            }),
            "the seed pane's child should spawn"
        );
        found.unwrap()
    };
    assert_eq!(
        server.status(&["wait", SESSION, "--until", &marker, "--timeout", "10"]),
        0,
        "the marker should appear on the pane before upgrade"
    );

    // Trigger the graceful upgrade.
    assert_eq!(server.status(&["upgrade"]), 0, "`phux upgrade` should ack");

    // The decisive check: the server re-execed IN PLACE, so its PID survives.
    // A kill+restart would not preserve it.
    assert!(
        poll(Duration::from_secs(10), || alive(server_pid)),
        "the server process (pid {server_pid}) must survive the in-place execve"
    );
    // The pane's child survived the upgrade with its master fd intact.
    assert!(
        alive(child_pid),
        "the pane's child (pid {child_pid}) must survive the upgrade"
    );

    // The resumed server rebuilt the tree + replayed the snapshot: reconnect
    // and confirm the session is back and the marker survived.
    assert!(
        poll(Duration::from_secs(15), || server.status(&["ls"]) == 0),
        "the resumed server should accept connections again"
    );
    let snap = server.stdout(&["snapshot", SESSION]);
    assert!(
        snap.contains(&marker),
        "the pane's scrollback marker should survive the upgrade; got:\n{snap}"
    );

    // Rebuilt actors must regain their exit watchers. Without that watcher a
    // kill stops the actor and child but never reaps the pane/window/session,
    // leaving a ghost row in `phux ls`. This harness never attaches a client,
    // so the server intentionally stays alive after becoming empty; the
    // authoritative session list is the reap assertion here.
    assert_eq!(
        server.status(&["kill", SESSION]),
        0,
        "the resumed session should accept a kill"
    );
    assert!(
        poll(Duration::from_secs(10), || !server
            .stdout(&["ls"])
            .contains(SESSION)),
        "the killed resumed session should be reaped from the authoritative list"
    );
    assert!(
        !alive(child_pid),
        "killing the resumed session should terminate its pane child"
    );
}
