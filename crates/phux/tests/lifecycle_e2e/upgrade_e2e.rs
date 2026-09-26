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
//! `just e2e` (or `cargo test -p phux --test lifecycle_e2e upgrade_e2e:: -- --ignored`).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SESSION: &str = "work";
const POLL: Duration = Duration::from_millis(50);

struct ServerGuard(common::ServerGuard);

impl std::ops::Deref for ServerGuard {
    type Target = common::ServerGuard;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ServerGuard {
    /// Spawn `phux server` with a seed pane running `seed_command`, then block
    /// until the socket appears.
    fn start_with_seed(seed_command: &str) -> Self {
        Self(
            common::ServerGuard::builder("upg")
                .seed_command(seed_command)
                .start(),
        )
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
    common::process_exists(pid)
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
    let server_pid = server.pid();

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
        server.status(&["kill", "--yes", SESSION]),
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
