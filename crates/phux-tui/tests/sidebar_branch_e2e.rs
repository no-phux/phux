//! End-to-end: the sidebar's VCS branch line derives from a real server's
//! `ATTACHED` snapshot cwd (spawn cwd -> `ResourceInfo::cwd` ->
//! `handle_server_frame` -> `VcsIndex` -> painted row), not client-side
//! injection. The fixture is a hand-written `.git/HEAD` whose branch name
//! appears nowhere else in the scenario.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use phux_client::snapshot::RenderedFrame;
use phux_protocol::wire::frame::AttachTarget;
use phux_tui::attach::run_headless_rendered;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, run_local, spawn_server_seed_pty_no_cmd, wait_for_socket,
};
use tempfile::TempDir;

/// The fixture branch: distinctive (in no path or command line) and short
/// enough to survive sidebar truncation.
const BRANCH: &str = "p4vp-e2e";

const SESSION: &str = "branch-e2e";

const VIEW: (u16, u16) = (80, 24);

/// The leftmost `width` columns of `frame` row `row`: the sidebar strip.
fn sidebar_row_text(frame: &RenderedFrame, row: u16, width: u16) -> String {
    let cols = usize::from(frame.cols);
    let base = usize::from(row) * cols;
    frame.cells[base..base + usize::from(width.min(frame.cols))]
        .iter()
        .map(|c| c.grapheme.as_str())
        .collect()
}

fn frame_shows_branch(frame: &RenderedFrame) -> bool {
    (0..frame.rows).any(|row| sidebar_row_text(frame, row, frame.cols).contains(BRANCH))
}

#[test]
fn sidebar_branch_line_derives_from_attached_snapshot_cwd() {
    // `run_headless_rendered` reads `[sidebar]` from the canonical config
    // path; enable it via a temp XDG home before any async machinery.
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

    // Canonicalized so the kernel-reported cwd matches (macOS /private/var).
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
        drop(wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await);

        // The cwd reaches the server only via the wire and is applied at
        // spawn, so the first attach's snapshot already carries it.
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
