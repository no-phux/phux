//! #2006: on Linux each pane runs in its own systemd scope, so systemd-oomd
//! or the memory controller kills a runaway pane, not the server and every
//! other pane with it. Needs a user bus; without one the test passes
//! vacuously unless `PHUX_TEST_REQUIRE_PANE_SCOPE=1` (soak.yml sets it).

#![cfg(target_os = "linux")]
#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::ServerGuard;

fn user_bus() -> bool {
    std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
        || std::env::var_os("XDG_RUNTIME_DIR")
            .is_some_and(|dir| Path::new(&dir).join("bus").exists())
}

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn a_pane_runs_in_its_own_systemd_scope() {
    if !user_bus() || std::env::var_os("PHUX_NO_PANE_SCOPE").is_some() {
        assert!(
            std::env::var_os("PHUX_TEST_REQUIRE_PANE_SCOPE").is_none(),
            "PHUX_TEST_REQUIRE_PANE_SCOPE is set but no user bus is reachable (or PHUX_NO_PANE_SCOPE is set)"
        );
        return;
    }
    let server = ServerGuard::builder("scope")
        .seed_command("exec sleep 600")
        .start();
    let end = Instant::now() + Duration::from_secs(15);
    let mut seen = String::new();
    while Instant::now() < end {
        let child = Command::new("pgrep")
            .args(["-P", &server.pid().to_string()])
            .output()
            .expect("run pgrep");
        let child = String::from_utf8_lossy(&child.stdout);
        if let Some(pid) = child.lines().next().map(str::trim) {
            seen = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
            if seen.contains(&format!("app-phux-pane-{pid}.scope")) {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the pane never moved into its own scope; its cgroup: {seen}");
}
