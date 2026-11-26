//! The exact local-owner raw-recipe review (`phux relaunch --approve-raw`)
//! driven through the real CLI on a pseudo-terminal, and the server
//! environment that review needs.

#![allow(
    dead_code,
    reason = "shared integration-test helpers are used per test binary"
)]
#![allow(unreachable_pub, reason = "shared by sibling integration-test crates")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "shared fail-fast integration-test harness"
)]

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::{POLL, ambient, server_guard::ServerSpawn, strip_terminal_controls};

/// The prompt the review prints after the complete recipe.
const PROMPT: &str = "[y/N]";

/// How long the review may take to print its recipe, and then to exit.
const REVIEW_DEADLINE: Duration = Duration::from_secs(30);

/// A server whose raw recipe a test will approve: a cleared environment with
/// only `PATH`, `TMPDIR`, a `/bin/sh` shell and a private home.
///
/// The proposal binds the server's whole inherited environment, which must
/// fit the bounded approval payload. A nix or direnv shell alone exceeds it,
/// and the host then refuses the review with nothing approved.
#[must_use]
pub fn hermetic_server(spawn: ServerSpawn) -> ServerSpawn {
    let mut spawn = spawn.env_clear().isolated_home();
    for key in ["PATH", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            spawn = spawn.env(key, value);
        }
    }
    spawn.env("SHELL", "/bin/sh").env("LANG", "C")
}

/// What a review printed and how it exited.
#[derive(Debug)]
pub struct Review {
    /// The exit status of `phux relaunch --approve-raw`.
    pub code: u32,
    /// Everything it printed, terminal controls removed.
    pub transcript: String,
}

/// `phux relaunch --socket SOCKET TARGET --approve-raw` on a PTY: wait for
/// the exact proposal and its prompt, type `answer`, and wait for the exit.
///
/// # Panics
///
/// When the prompt or the exit does not come within [`REVIEW_DEADLINE`].
pub fn review(socket: &Path, target: &str, answer: &str) -> Review {
    let pty = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 24,
            cols: 200,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut cmd = portable_pty::CommandBuilder::new(crate::runner::phux_bin());
    for key in ambient::ambient_phux_keys() {
        cmd.env_remove(key);
    }
    let xdg = ambient::isolated_xdg();
    cmd.env("XDG_STATE_HOME", &xdg.state);
    cmd.env("XDG_CONFIG_HOME", &xdg.config);
    cmd.env("TERM", "xterm-256color");
    cmd.env("RUST_LOG", "off");
    cmd.env(ambient::NO_UPDATE_CHECK_ENV, ambient::NO_UPDATE_CHECK_VALUE);
    cmd.arg("relaunch");
    cmd.arg("--socket");
    cmd.arg(socket);
    cmd.args([target, "--approve-raw"]);
    let mut child = pty.slave.spawn_command(cmd).expect("spawn phux relaunch");
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader().expect("clone reader");
    let mut writer = pty.master.take_writer().expect("pty writer");
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0_u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut seen = Vec::new();
    let drain = |seen: &mut Vec<u8>| {
        while let Ok(chunk) = rx.try_recv() {
            seen.extend(chunk);
        }
    };
    let start = Instant::now();
    loop {
        drain(&mut seen);
        if strip_terminal_controls(&seen).contains(PROMPT) {
            break;
        }
        if let Some(status) = child.try_wait().expect("poll review") {
            panic!(
                "review exited {} before asking: {}",
                status.exit_code(),
                strip_terminal_controls(&seen)
            );
        }
        assert!(
            start.elapsed() < REVIEW_DEADLINE,
            "review never asked: {}",
            strip_terminal_controls(&seen)
        );
        std::thread::sleep(POLL);
    }
    writer
        .write_all(format!("{answer}\n").as_bytes())
        .expect("type the answer");
    writer.flush().expect("flush the answer");
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll review") {
            break status;
        }
        if start.elapsed() > REVIEW_DEADLINE {
            let _ = child.kill();
            panic!("review did not exit: {}", strip_terminal_controls(&seen));
        }
        drain(&mut seen);
        std::thread::sleep(POLL);
    };
    // The exit may race the last bytes through the PTY.
    std::thread::sleep(POLL);
    drain(&mut seen);
    Review {
        code: status.exit_code(),
        transcript: strip_terminal_controls(&seen),
    }
}

/// [`review`] answering yes, which must commit: the transcript shows the
/// complete proposal and the committed relaunch.
pub fn approve(socket: &Path, target: &str) -> Review {
    let review = review(socket, target, "y");
    assert_eq!(
        review.code, 0,
        "approved review failed: {}",
        review.transcript
    );
    assert!(
        review
            .transcript
            .contains("Exact archived execution proposal"),
        "the review must print the exact proposal first: {}",
        review.transcript
    );
    review
}
