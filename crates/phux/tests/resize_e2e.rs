//! Binary-level end-to-end tests for `phux resize`: the headless way to set
//! a pane's grid.
//!
//! What these prove that a unit test cannot: the size the verb reports is the
//! size **libghostty actually settled on**. `phux resize` reads its answer
//! back out of the registry (`GET_STATE`), which is server bookkeeping; the
//! assertions here read it back out of `phux snapshot --json`, which is
//! `GET_SCREEN` — a projection the pane actor builds from its own live
//! `Terminal` after `Terminal::resize` and the `TIOCSWINSZ` ioctl. If the two
//! ever disagree — a clamped dimension, a resize the actor dropped — this
//! lane fails and the unit lane does not.
//!
//! Harness discipline follows `rec_e2e.rs`: a real `phux server` child on a
//! private UDS at the root of `/tmp` (macOS caps `sun_path` at 104 bytes and
//! these are usually run by hand from a deep worktree), each verb its own
//! subprocess, guard-killed and unlinked on drop.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Idle lifetime for this file's harness server, as a backstop UNDER the
/// `Drop` kill (ADR-0063). The guard is still the primary cleanup; it cannot
/// run if the test process is `SIGKILL`ed or the runner is reaped mid-job, and
/// what leaks then is a daemon holding a live PTY on a socket nobody will
/// ever look at again. Ten minutes is far longer than any gap between this
/// file's client connections, so it can only fire after the harness is gone.
const SERVER_IDLE_LIMIT_SECS: &str = "600";

/// Path to the freshly-built `phux` binary, injected by cargo.
const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// The pre-seeded session name every test drives against.
const SESSION: &str = "work";

/// The grid a pane gets with nobody attached. Every assertion below is
/// written against a size that is NOT this, so none of them can pass by
/// coincidence.
const NO_TTY_DEFAULT: (u64, u64) = (80, 24);

/// How long to wait for the server to bind its socket (cold-start bound).
const SOCKET_DEADLINE: Duration = Duration::from_secs(30);

/// Poll cadence for the socket wait.
const POLL: Duration = Duration::from_millis(50);

/// Monotonic counter so concurrent tests never collide on a socket path.
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A running `phux server`, killed and unlinked when the guard drops.
struct ServerGuard {
    child: Child,
    socket: PathBuf,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // The server unlinks on clean shutdown; we SIGKILL it, so a stale
        // socket in /tmp would otherwise outlive every run.
        let _ = std::fs::remove_file(&self.socket);
    }
}

impl ServerGuard {
    fn start() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = PathBuf::from(format!(
            "/tmp/phux-resize-e2e-{}-{n}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&socket);
        let child = Command::new(PHUX)
            .args(["server", "--session", SESSION, "--socket"])
            .arg(&socket)
            .args(["--exit-after-idle", SERVER_IDLE_LIMIT_SECS])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server");
        let guard = Self { child, socket };
        let deadline = Instant::now() + SOCKET_DEADLINE;
        while Instant::now() < deadline {
            if guard.socket.exists() {
                return guard;
            }
            std::thread::sleep(POLL);
        }
        panic!(
            "phux server did not bind {} within {SOCKET_DEADLINE:?}",
            guard.socket.display()
        );
    }

    /// Build `phux <verb> --socket <sock> <rest...>`. `--socket` goes right
    /// after the verb, matching the sibling e2e harnesses.
    fn cmd(&self, args: &[&str]) -> Command {
        let (verb, rest) = args.split_first().expect("at least a verb");
        let mut c = Command::new(PHUX);
        c.arg(verb)
            .arg("--socket")
            .arg(&self.socket)
            .args(rest)
            .stdin(Stdio::null());
        c
    }

    /// Run a verb, returning `(exit code, stdout, stderr)`.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd(args).output().expect("run phux verb");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn success(&self, args: &[&str]) -> String {
        let (code, stdout, stderr) = self.run(args);
        assert_eq!(code, 0, "phux {args:?} exited {code}; stderr={stderr}");
        stdout
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
