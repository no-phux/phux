//! A harness-spawned TUI must not paint the "newer phux is available" toast,
//! even when the state dir already caches a newer release.
//!
//! v0.82.0 made `plugin_overlay_and_sidebar_section_drive_a_real_tui` fail
//! three times on
//! <https://github.com/no-phux/phux/actions/runs/38922955520>: the toast ate
//! the `e` in `echo`, and the shell saw `cho`. `PHUX_NO_UPDATE_CHECK` skips
//! the live lookup and suppresses a cached notice. This plants that cache
//! (fresh, same build, so a missed opt-out would still paint `v9.9.9` and
//! would not reach the network) and drives a real attach through `PtyAttach`.
//!
//! `phux` and `phux-tui` share the workspace version. The cache `current`
//! field has to equal that version, which is what the toast compares
//! against, or it would stay down even with the check enabled.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::time::{Duration, Instant};

use portable_pty::CommandBuilder;

const SESSION: &str = "work";
const DEADLINE: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(100);
/// How long the status bar keeps polling a background update check.
const UPDATE_POLL_WINDOW: Duration = Duration::from_secs(8);
/// A version no real release uses, so a painted toast can only be this stub.
const STUB_LATEST: &str = "v9.9.9";

/// Wait until every phrase has been painted; panic with the transcript.
fn wait_for(client: &common::PtyAttach, phrases: &[&str]) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let painted = client.painted();
        if phrases.iter().all(|phrase| painted.contains(phrase)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "never painted {phrases:?}; painted text was:\n{painted}"
        );
        std::thread::sleep(POLL);
    }
}

/// Plant a completed onboarding file and a fresh update-check cache that
/// names [`STUB_LATEST`], then point the attach at that state dir.
fn plant_stubbed_release(config: &std::path::Path, command: &mut CommandBuilder) {
    let state_root = config.join("state");
    let phux_state = state_root.join("phux");
    std::fs::create_dir_all(&phux_state).expect("state dir");
    std::fs::write(
        phux_state.join("onboarding.json"),
        r#"{"version":1,"stage":"complete"}"#,
    )
    .expect("preseed completed onboarding state");
    let checked_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    // Same string `phux-tui` bakes in as the running version.
    let version = env!("CARGO_PKG_VERSION");
    let cache = serde_json::json!({
        "checked_at": checked_at,
        "current": version,
        "available": {
            "current": version,
            "latest": STUB_LATEST,
            "command": "phux update",
        },
        "notified": null,
    });
    std::fs::write(phux_state.join("update-check.json"), cache.to_string()).expect("cache");
    command.env("PHUX_PROFILE", "default");
    command.env("XDG_STATE_HOME", &state_root);
}

#[test]
#[ignore = "spawns a real server and attached PTY client; run in the e2e lane"]
fn harness_attach_hides_a_stubbed_newer_release() {
    let server = common::ServerGuard::builder("update-notice")
        .env("SHELL", "/bin/sh")
        .start();
    let mut client =
        common::PtyAttach::start_with(&server.socket, &[SESSION], (120, 40), plant_stubbed_release);
    wait_for(&client, &["Sessions"]);

    // The bar polls the cache once a second for this long. The stub is
    // already on disk, so the first tick would paint it if the opt-out
    // failed to suppress a cached notice.
    let deadline = Instant::now() + UPDATE_POLL_WINDOW;
    loop {
        let painted = client.painted();
        assert!(
            !painted.contains(STUB_LATEST),
            "update notice painted under the harness:\n{painted}"
        );
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(POLL);
    }

    client.send(b"echo STILL-$((4+4))\r");
    wait_for(&client, &["STILL-8"]);
    assert!(
        client.is_running(),
        "the attach exited while proving the update notice stays down"
    );
}
