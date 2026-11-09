//! #1956: every graceful upgrade leaked one PTY master per adopted pane, so a
//! long-lived server filled its descriptor table. Repeated upgrades with
//! several panes must leave the count where the first upgrade left it.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use common::ServerGuard;

const UPGRADES: usize = 4;
const SESSIONS: [&str; 3] = ["work", "fd-two", "fd-three"];

/// The server's open descriptors: `/proc` on Linux, `lsof` elsewhere.
fn open_descriptors(pid: u32) -> usize {
    if let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) {
        return entries.count();
    }
    let out = Command::new("lsof")
        .args(["-n", "-P", "-p", &pid.to_string(), "-F", "f"])
        .output()
        .expect("run lsof");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.starts_with('f'))
        .count()
}

fn poll(deadline: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// `phux upgrade`, then wait until the re-execed server answers again.
fn upgrade(server: &ServerGuard) {
    assert_eq!(server.run(&["upgrade"]).0, 0, "`phux upgrade` should ack");
    assert!(
        poll(Duration::from_secs(15), || server.run(&["ls"]).0 == 0),
        "the resumed server should accept connections again"
    );
    assert!(
        common::process_exists(server.pid()),
        "the server re-execed in place"
    );
}

#[test]
#[ignore = "spawns a real phux server + performs real in-place re-execs; run via `just e2e`."]
fn repeated_upgrades_do_not_leak_descriptors() {
    let server = ServerGuard::builder("upgfd")
        .seed_command("exec sleep 600")
        .start();
    let pid = server.pid();
    for session in &SESSIONS[1..] {
        server.success(&["new", "--json", "-s", session, "--", "sleep", "600"]);
    }

    // The first upgrade sets the baseline: a cold start opens some
    // descriptors lazily.
    upgrade(&server);
    std::thread::sleep(Duration::from_secs(1));
    let baseline = open_descriptors(pid);
    for _ in 0..UPGRADES {
        upgrade(&server);
    }
    // A leak grows by one per pane per upgrade; allow a couple of
    // descriptors a just-closed client may still hold.
    let mut last = 0;
    assert!(
        poll(Duration::from_secs(10), || {
            last = open_descriptors(pid);
            last <= baseline + 2
        }),
        "{UPGRADES} upgrades grew the server's descriptors from {baseline} to {last}"
    );
    let listed = server.success(&["ls"]);
    for session in SESSIONS {
        assert!(listed.contains(session), "{session} survived: {listed}");
    }
}
