//! Real-server coverage for the plugin TUI surfaces: an `overlay` pane
//! (ADR-0147) and a `[[sidebar]]` section (ADR-0148), driven through a real
//! attached TUI on a pseudo-terminal.
//!
//! The overlay must paint its own output in a titled box, take the keyboard
//! while open, and hand it back to the layout pane both when `kill-pane`
//! dismisses it and when its process exits, without ending the attach.
//! Markers are typed as `$((…))` arithmetic so the painted transcript holds
//! the shell's answer only when the keystrokes reached that shell.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::time::{Duration, Instant};

const SESSION: &str = "work";
const DEADLINE: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(100);
/// The prefix chord, `C-a`.
const PREFIX: &[u8] = b"\x01";

const MANIFEST: &str = r#"
id = "e2e.surfaces"
name = "E2E Surfaces"
version = "0.1.0"
min_phux_version = "0.0.2"

[[panes]]
id = "stay"
title = "Stay Board"
placement = "overlay"
command = ["sh", "-c", "echo $$ > overlay.pid; printf 'OVERLAY-%s\n' $((20+22)); exec sleep 300"]

[[panes]]
id = "brief"
title = "Brief Board"
placement = "overlay"
command = ["sh", "-c", "printf 'BRIEF-%s\n' $((30+3)); sleep 1"]

[[sidebar]]
id = "panes"
title = "E2E Panes"
format = "{index}:{window}"
rows = 2
"#;

const CONFIG: &str = r#"
[[plugins]]
manifest = "plugin/phux-plugin.toml"

[keybindings.prefix-table]
"O" = { action = "plugin-pane", plugin = "e2e.surfaces", pane = "stay" }
"B" = { action = "plugin-pane", plugin = "e2e.surfaces", pane = "brief" }
x = "kill-pane"
"#;

/// A TUI attached at 120x40 with the plugin linked and onboarding done.
fn attach(server: &common::ServerGuard) -> common::PtyAttach {
    common::PtyAttach::start_with(&server.socket, &[SESSION], (120, 40), |config, command| {
        let phux = config.join("phux");
        std::fs::create_dir_all(phux.join("plugin")).expect("plugin dir");
        std::fs::write(phux.join("plugin/phux-plugin.toml"), MANIFEST).expect("manifest");
        std::fs::write(phux.join("config.toml"), CONFIG).expect("config");
        std::fs::write(
            phux.join("onboarding.json"),
            r#"{"version":1,"stage":"complete"}"#,
        )
        .expect("preseed completed onboarding state");
        command.env("PHUX_PROFILE", "default");
        command.env("XDG_STATE_HOME", config);
    })
}

/// Whether the `stay` overlay's process still runs. It records its pid in
/// its working directory, the plugin root, before `exec`ing.
fn overlay_running(client: &common::PtyAttach) -> bool {
    let pid = std::fs::read_to_string(client.config.path().join("phux/plugin/overlay.pid"))
        .expect("the overlay recorded its pid");
    std::process::Command::new("sh")
        .args(["-c", &format!("kill -0 {}", pid.trim())])
        .status()
        .is_ok_and(|status| status.success())
}

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

#[test]
#[ignore = "spawns a real server and attached PTY client; run in the e2e lane"]
fn plugin_overlay_and_sidebar_section_drive_a_real_tui() {
    let server = common::ServerGuard::builder("plugin-surfaces")
        .env("SHELL", "/bin/sh")
        .start();
    let mut client = attach(&server);
    // The sidebar section is laid out as soon as the strip paints.
    wait_for(&client, &["E2E Panes", "Sessions"]);

    // Open the overlay: its output paints inside its titled box.
    client.send(PREFIX);
    client.send(b"O");
    wait_for(&client, &["Stay Board", "OVERLAY-42"]);

    // Keys go to the overlay (whose `sleep` ignores them), so the layout
    // shell must not answer this one.
    client.send(b"echo SHELL-$((5+5))\r");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !client.painted().contains("SHELL-10"),
        "keys reached the layout pane under an open overlay:\n{}",
        client.painted()
    );

    // `kill-pane` dismisses the overlay only; the shell gets input back.
    assert!(overlay_running(&client), "the overlay's process is running");
    client.send(PREFIX);
    client.send(b"x");
    std::thread::sleep(Duration::from_millis(300));
    client.send(b"echo BACK-$((6+6))\r");
    wait_for(&client, &["BACK-12"]);
    assert!(
        client.is_running(),
        "dismissing the overlay ended the attach"
    );
    // The dismissal killed the overlay's Terminal, and its process with it.
    let deadline = Instant::now() + DEADLINE;
    while overlay_running(&client) {
        assert!(
            Instant::now() < deadline,
            "dismissing left the overlay's process running; server listing:\n{}",
            server.success(&["ls", "--json"])
        );
        std::thread::sleep(POLL);
    }

    // An overlay whose process exits closes by itself.
    client.send(PREFIX);
    client.send(b"B");
    wait_for(&client, &["Brief Board", "BRIEF-33"]);
    std::thread::sleep(Duration::from_secs(2));
    client.send(b"echo AFTER-$((7+7))\r");
    wait_for(&client, &["AFTER-14"]);
    assert!(client.is_running(), "an exiting overlay ended the attach");
}
