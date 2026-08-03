//! phux-p4vp — end-to-end: the sidebar's VCS branch line derives from
//! REAL `ATTACHED` snapshot cwds, not client-side injection.
//!
//! Prior client coverage fed the branch machinery from the client side:
//! `vcs` tests derive branches from fixture repos given a cwd directly,
//! and the sidebar tests hand `WindowInfo` a pre-derived branch string.
//! Nothing proved that a server-populated `TerminalInfo::cwd` actually
//! flows through `handle_server_frame` -> `VcsIndex` -> `window_infos`
//! -> the painted branch row. This test closes that seam with the full
//! path, one process end to end:
//!
//! 1. A real `ServerRuntime` on a UDS with PTY-backed attach-create.
//! 2. `AttachTarget::CreateIfMissing` seeds a blocked shell (`read _`
//!    keeps the child alive on the PTY) with the fixture git repo as
//!    its wire `cwd` (honored server-side since phux-3mtf; the test
//!    previously worked around the gap with an in-command `cd`).
//! 3. The server's `ATTACHED` snapshot carries the pane's kernel cwd
//!    (spawn-time stamp + attach-time `refresh_registry_cwds`, the
//!    phux-p4vp server fix).
//! 4. `run_headless_rendered` replays that snapshot through the real
//!    client frame handler and composites the sidebar.
//! 5. The branch name appears on a sidebar row of the rendered frame.
//!
//! The fixture repo is a hand-written `.git/HEAD` (no `git` subprocess —
//! matching how `phux_client::vcs` reads it). The branch name is chosen
//! so it appears nowhere else in the scenario (not in any path, command
//! line, or shell echo), so finding it in the frame proves the
//! derivation ran against the wire-carried cwd.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use phux_client::attach::run_headless_rendered;
use phux_client::snapshot::RenderedFrame;
use phux_protocol::wire::frame::AttachTarget;
// The server fixture, the connect-poll, and the `LocalSet` bootstrap used to
// be hand-copied into this file. They are the testkit's versions verbatim,
// down to the 10s connect ceiling (`SOCKET_CONNECT_DEADLINE`), so the copies
// were pure drift risk.
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, run_local, spawn_server_seed_pty_no_cmd, wait_for_socket,
};
use tempfile::TempDir;

/// Branch name written into the fixture `HEAD`. Deliberately distinctive:
/// it must not be a substring of any temp path, command line, or shell
/// prompt the composited panes could echo. Short enough (8 cells) to
/// survive the sidebar's width-based truncation at the default width 20.
const BRANCH: &str = "p4vp-e2e";

/// Session name for the attach-created session.
const SESSION: &str = "branch-e2e";

/// Composite viewport. Sidebar (default width 20, left) + panes.
const VIEW: (u16, u16) = (80, 24);

/// The leftmost `width` columns of `frame` row `row`, joined as text —
/// the sidebar strip occupies exactly those cells (left edge, default).
fn sidebar_row_text(frame: &RenderedFrame, row: u16, width: u16) -> String {
    let cols = usize::from(frame.cols);
    let base = usize::from(row) * cols;
    frame.cells[base..base + usize::from(width.min(frame.cols))]
        .iter()
        .map(|c| c.grapheme.as_str())
        .collect()
}

/// `true` when any sidebar row of `frame` carries the fixture branch.
fn frame_shows_branch(frame: &RenderedFrame) -> bool {
    (0..frame.rows).any(|row| sidebar_row_text(frame, row, 20).contains(BRANCH))
}

#[test]
fn sidebar_branch_line_derives_from_attached_snapshot_cwd() {
    // `run_headless_rendered` reads `[sidebar]` via the canonical config
    // path; point XDG at a temp home that enables it. Must happen before
    // any async machinery spins up.
    let cfg_home = TempDir::new().unwrap();
    let phux_cfg_dir = cfg_home.path().join("phux");
    std::fs::create_dir_all(&phux_cfg_dir).unwrap();
    std::fs::write(
        phux_cfg_dir.join("config.toml"),
        "[sidebar]\nenabled = true\n",
    )
    .unwrap();
    // SAFETY: process-global env mutation before any thread exists (the
    // tokio runtime is built below). This file holds a single test, so no
    // sibling test races it under `cargo test` either; nextest isolates
    // per-process regardless. Same pattern as `phux-server/tests/ws_attach.rs`.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", cfg_home.path());
    }

    // Fixture repo: a hand-written `.git/HEAD` on a branch, exactly the
    // shape `phux_client::vcs` derives from. Canonicalize so the shell's
    // `cd` target and the kernel-reported cwd agree in spelling (macOS
    // resolves /var -> /private/var).
    let repo = TempDir::new().unwrap();
    let repo_path = repo.path().canonicalize().expect("canonicalize repo");
    std::fs::create_dir_all(repo_path.join(".git")).unwrap();
    std::fs::write(
        repo_path.join(".git/HEAD"),
        format!("ref: refs/heads/{BRANCH}\n"),
    )
    .unwrap();

    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server_seed_pty_no_cmd(socket_path.clone(), None);
        // The probe connection is dropped immediately: this only proves the
        // listener is bound, and `run_headless_rendered` below dials its own.
        drop(wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await);

        // The seed shell blocks on the PTY inside the fixture repo,
        // reached via the wire `cwd` (phux-3mtf). The snapshot cwd must
        // come from the wire — nothing client-side knows this path.
        //
        // No retry loop: the cwd is applied at spawn time (portable_pty
        // chdirs the child before exec), so unlike the pre-3mtf
        // workaround — an in-command `cd` racing the attach-time kernel
        // query — the FIRST attach's snapshot already carries the repo
        // directory (both the spawn-time stamp and the kernel refresh
        // resolve to it).
        let target = AttachTarget::CreateIfMissing {
            name: SESSION.to_owned(),
            command: Some(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "read _".to_owned(),
            ]),
            cwd: Some(repo_path.display().to_string()),
        };

        let frame = run_headless_rendered(&socket_path, target, VIEW.0, VIEW.1)
            .await
            .expect("headless rendered attach");

        if !frame_shows_branch(&frame) {
            let dump: Vec<String> = (0..frame.rows)
                .map(|r| sidebar_row_text(&frame, r, frame.cols))
                .collect();
            panic!(
                "sidebar did not show branch {BRANCH:?}; composited frame:\n{}",
                dump.join("\n"),
            );
        }

        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}
