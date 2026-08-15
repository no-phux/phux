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
//! The same read-back is what proves the attach path's chrome reservation
//! (phux-e9fd): a real `phux attach` in a pseudoterminal must leave the pane's
//! grid one row SHORT of the PTY, because the client spends that row on the
//! status bar. Only `GET_SCREEN` can tell "the PTY agreed to 23 rows" from
//! "the PTY is 24 rows and the client paints over the last one".
//!
//! Harness discipline follows `rec_e2e.rs`: a real `phux server` child on a
//! private UDS at the root of `/tmp` (macOS caps `sun_path` at 104 bytes and
//! these are usually run by hand from a deep worktree), each verb its own
//! subprocess, guard-killed and unlinked on drop.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

mod common;

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

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
    _process: common::ServerProcess,
    socket: PathBuf,
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
        let guard = Self {
            _process: common::ServerProcess::from_child(child, socket.clone()),
            socket,
        };
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

/// The PTY the attached client in this file's chrome test runs in.
const ATTACH_PTY: (u16, u16) = (100, 24);

/// How long an attach gets to hand the server its post-chrome pane size.
const ATTACH_DEADLINE: Duration = Duration::from_secs(20);

/// A real `phux attach` running in a pseudoterminal, killed on drop.
///
/// Nothing is typed into it: the whole point is what the client does on its
/// own between `ATTACH` and the first idle frame.
struct AttachedClient {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _config: tempfile::TempDir,
}

impl Drop for AttachedClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl AttachedClient {
    /// Attach to `server` through a `cols x rows` PTY under an empty
    /// `XDG_CONFIG_HOME`, so the client runs on the embedded `default.toml` —
    /// which ships a bottom `[status]` bar. That default is load-bearing here:
    /// with no bar there is no reserved row and nothing to get wrong.
    fn start(server: &ServerGuard, (cols, rows): (u16, u16)) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open attach PTY");
        let config = tempfile::tempdir().expect("isolated config dir");
        let mut command = CommandBuilder::new(PHUX);
        command.args([
            "attach",
            "--socket",
            server.socket.to_str().expect("UTF-8 socket"),
            SESSION,
        ]);
        command.env("SHELL", "/bin/sh");
        command.env("TERM", "xterm-256color");
        command.env("RUST_LOG", "off");
        command.env("XDG_CONFIG_HOME", config.path());
        let child = pair
            .slave
            .spawn_command(command)
            .expect("spawn attached TUI");
        drop(pair.slave);

        // Drain the paint stream so a full PTY buffer can never backpressure
        // the client into stalling before it emits its reflow.
        let mut reader = pair.master.try_clone_reader().expect("clone PTY reader");
        std::thread::spawn(move || {
            let mut bytes = [0u8; 8192];
            while let Ok(read) = reader.read(&mut bytes) {
                if read == 0 {
                    break;
                }
            }
        });
        Self {
            child,
            _config: config,
        }
    }
}

#[test]
#[ignore = "spawns a real phux server and an attached PTY client; run via `just e2e`."]
fn attach_sizes_the_pane_to_the_viewport_minus_the_status_bar() {
    // phux-e9fd. The server sizes each pane from `ATTACH.viewport`, which is
    // the client's OUTER terminal — status bar included. The client paints
    // panes into the content rect, one row shorter. Nothing reconciled the
    // two at attach, so the pane's bottom line lived on a row the client
    // never painted and the bar appeared to have eaten it. It "fixed itself"
    // on the next resize/split/sidebar toggle purely because those paths do
    // emit `TERMINAL_RESIZE`.
    let server = ServerGuard::start();
    assert_eq!(
        server.pane_size(),
        NO_TTY_DEFAULT,
        "the seeded pane must start at the no-TTY default, or the assertion \
         below could pass without the attach doing anything"
    );

    let _client = AttachedClient::start(&server, ATTACH_PTY);

    let (cols, rows) = ATTACH_PTY;
    // phux-k0cw: the content rect is narrower as well as shorter now — the
    // window sidebar ships enabled, so it reserves its columns on the same
    // attach. Both axes are the same reconciliation, so assert the whole
    // content rect rather than only the row this test was written for.
    // Read from the shipped default rather than hardcoded, so changing the
    // width in one place does not silently leave this asserting the old one.
    let sidebar = phux_config::SidebarCfg::default();
    let want = (
        u64::from(cols) - u64::from(sidebar.width),
        u64::from(rows) - 1,
    );
    let deadline = Instant::now() + ATTACH_DEADLINE;
    let mut seen = server.pane_size();
    while Instant::now() < deadline && seen != want {
        std::thread::sleep(POLL);
        seen = server.pane_size();
    }
    assert_eq!(
        seen,
        want,
        "an attached client on a {cols}x{rows} PTY reserves one row for the \
         status bar and {sidebar_width} columns for the window sidebar, so \
         the pane's real grid must settle at {want:?}. Seeing the full {rows} \
         rows or {cols} columns means the client never sent the post-attach \
         TERMINAL_RESIZE: the PTY is larger than the rect the client paints \
         into, so the shell renders into cells that are clipped away and the \
         chrome looks like it overwrote them.",
        sidebar_width = sidebar.width
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
