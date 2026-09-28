//! `phux resize` against a real server: the reported size must match what
//! libghostty settled on, read back through `phux snapshot --json`
//! (`GET_SCREEN`), not the registry. The same read-back proves a real
//! `phux attach` leaves the grid one row short of the PTY for the status bar.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Path to the freshly-built `phux` binary, injected by cargo.
const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// The pre-seeded session name every test drives against.
const SESSION: &str = "work";

/// The grid a pane gets with nobody attached. Every assertion below is
/// written against a size that is NOT this, so none of them can pass by
/// coincidence.
const NO_TTY_DEFAULT: (u64, u64) = (80, 24);

/// Poll cadence for the chrome-reservation wait.
const POLL: Duration = Duration::from_millis(50);

/// A running `phux server`, killed and unlinked when the guard drops.
struct ServerGuard(common::ServerGuard);

impl std::ops::Deref for ServerGuard {
    type Target = common::ServerGuard;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ServerGuard {
    fn start() -> Self {
        Self(
            common::ServerGuard::builder("resize")
                // Panes run the server's `$SHELL`; never inherit the runner's.
                .env("SHELL", "/bin/sh")
                .start(),
        )
    }

    /// The pane's grid as the **pane actor's own libghostty `Terminal`**
    /// reports it — `GET_SCREEN`, not the registry field `phux resize`
    /// reads back. That difference is the point of this file.
    fn pane_size(&self) -> (u64, u64) {
        let stdout = self.success(&["snapshot", "--json", SESSION]);
        let snapshot: serde_json::Value =
            serde_json::from_str(&stdout).expect("snapshot --json must be JSON");
        (
            snapshot["cols"].as_u64().expect("snapshot cols"),
            snapshot["rows"].as_u64().expect("snapshot rows"),
        )
    }
}

/// The PTY the attached client in this file's chrome test runs in.
const ATTACH_PTY: (u16, u16) = (100, 24);

/// How long an attach gets to hand the server its post-chrome pane size.
const ATTACH_DEADLINE: Duration = Duration::from_secs(20);

#[test]
#[ignore = "spawns a real phux server and an attached PTY client; run via `just e2e`."]
fn attach_sizes_the_pane_to_the_viewport_minus_the_status_bar() {
    // `ATTACH.viewport` is the outer terminal; the client must resize the
    // pane to the content rect, one row shorter for the status bar.
    let server = ServerGuard::start();
    assert_eq!(
        server.pane_size(),
        NO_TTY_DEFAULT,
        "the seeded pane must start at the no-TTY default, or the assertion \
         below could pass without the attach doing anything"
    );

    // An empty config runs the embedded defaults, which ship the status bar.
    let _client = common::PtyAttach::start(&server.socket, &[SESSION], ATTACH_PTY, &[]);

    let (cols, rows) = ATTACH_PTY;
    // phux-k0cw: the content rect is narrower as well as shorter now — the
    // window sidebar ships enabled, so it reserves its columns on the same
    // attach. Both axes are the same reconciliation, so assert the whole
    // content rect rather than only the row this test was written for.
    // Read from the shipped default rather than hardcoded, so changing the
    // width in one place does not silently leave this asserting the old one.
    let sidebar = phux_config::SidebarCfg::default();
    let sidebar_width = if sidebar.width == 0 {
        (cols / 4).clamp(28, 40)
    } else {
        sidebar.width
    };
    // Two rows come off the top and bottom of the viewport: the status bar
    // and the pane-title rail the chrome draws above every pane grid
    // (phux-l96p.8).
    let want = (
        u64::from(cols) - u64::from(sidebar_width),
        u64::from(rows) - 2,
    );
    let deadline = Instant::now() + ATTACH_DEADLINE;
    let mut seen = server.pane_size();
    while Instant::now() < deadline && seen != want {
        std::thread::sleep(POLL);
        seen = server.pane_size();
    }
    assert_eq!(
        seen, want,
        "an attached client on a {cols}x{rows} PTY reserves one row for the \
         status bar, one row for the pane-title rail, and {sidebar_width} \
         columns for the window sidebar, so \
         the pane's real grid must settle at {want:?}. Seeing the full {rows} \
         rows or {cols} columns means the client never sent the post-attach \
         RESIZE_TERMINAL: the PTY is larger than the rect the client paints \
         into, so the shell renders into cells that are clipped away and the \
         chrome looks like it overwrote them."
    );
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn resize_changes_the_panes_real_grid() {
    let server = ServerGuard::start();
    assert_eq!(
        server.pane_size(),
        NO_TTY_DEFAULT,
        "a freshly seeded pane must start at the no-TTY default, or the \
         assertions below could pass without the resize doing anything"
    );

    let stdout = server.success(&["resize", SESSION, "120x40"]);
    assert_eq!(
        stdout.trim(),
        "120x40",
        "the plain output is the size the server holds, so a script can read \
         it without --json"
    );
    assert_eq!(
        server.pane_size(),
        (120, 40),
        "`phux resize` reported success but the pane actor's own grid did \
         not move. The registry `dims` the verb reads back and the \
         libghostty `Terminal` the snapshot projects have diverged: look for \
         a dropped ResizeRequest on the actor's resize mailbox, or a \
         clamp applied on one side of that boundary and not the other."
    );

    // Shrinking is the direction that historically broke libghostty's
    // `PageList` reflow, and it is also the direction a caller uses to undo
    // an over-large grid. Prove the verb is not one-way.
    server.success(&["resize", SESSION, "90x30"]);
    assert_eq!(server.pane_size(), (90, 30));
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn resize_json_reports_requested_and_applied() {
    let server = ServerGuard::start();
    let stdout = server.success(&["resize", SESSION, "--json", "200x50"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("resize --json must be JSON");

    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["requested"]["cols"], 200);
    assert_eq!(doc["requested"]["rows"], 50);
    assert_eq!(doc["applied"]["cols"], 200);
    assert_eq!(doc["applied"]["rows"], 50);
    assert_eq!(
        doc["held"], true,
        "with nobody attached there is no viewport to lose to: {doc}"
    );
    assert_eq!(server.pane_size(), (200, 50));
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn resize_does_not_attach_the_session() {
    // The trap this verb exists to avoid falling into itself: a headless
    // caller that attaches contributes an 80x24 no-TTY viewport, and under
    // the default `window-size = "smallest"` policy that drags the pane
    // straight back to the default it was just moved off. Resizing twice in
    // a row is what makes a lingering subscription observable — the second
    // call's read-back would show 80x24, not 140x45.
    let server = ServerGuard::start();
    server.success(&["resize", SESSION, "140x45"]);
    let stdout = server.success(&["resize", SESSION, "140x45"]);
    assert_eq!(stdout.trim(), "140x45");
    assert_eq!(
        server.pane_size(),
        (140, 45),
        "a previous `phux resize` left a view attached to the session; its \
         80x24 no-TTY viewport is now fighting the size being requested"
    );
}

#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn resize_refuses_an_unknown_target_without_touching_any_pane() {
    let server = ServerGuard::start();
    server.success(&["resize", SESSION, "110x35"]);

    let (code, stdout, stderr) = server.run(&["resize", "no-such-session", "60x20"]);
    assert_ne!(code, 0, "an unresolvable target must exit nonzero");
    assert!(
        stdout.is_empty(),
        "a target miss must leave stdout clean for the script: {stdout}"
    );
    assert!(
        stderr.contains("no such target"),
        "the diagnostic must name the miss: {stderr}"
    );
    assert_eq!(
        server.pane_size(),
        (110, 35),
        "a failed resize must not have moved some other pane"
    );
}

#[test]
fn resize_rejects_a_zero_axis_before_it_reaches_a_server() {
    // No `ServerGuard`: clap's value parser rejects this, so the verb never
    // opens a socket. Pointing it at a path with no server proves that — if
    // the geometry check moved server-side, this would fail with a
    // connection error instead of a usage error.
    let out = Command::new(PHUX)
        .args([
            "resize",
            "--socket",
            "/tmp/phux-resize-e2e-nonexistent.sock",
            SESSION,
            "0x40",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("run phux resize");
    assert_ne!(out.status.code(), Some(0), "0 columns must be rejected");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("at least 1"),
        "the diagnostic must say why zero is wrong, not just that it is: {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "a usage error must leave stdout empty"
    );
}
