//! Binary-level end-to-end tests for `phux server --exit-after-idle`
//! (ADR-0063): the opt-in lifetime that lets an ephemeral caller bound a
//! daemon it may never get to clean up.
//!
//! `crates/phux-server/tests/server_idle_exit.rs` proves the runtime rule
//! in-process. This file proves the three things only a real process can:
//!
//!   1. the CLI flag actually reaches `ServerConfig` (a broken wire-through
//!      would leave that in-process test perfectly green);
//!   2. the daemon **process** goes away, not merely `run_async` returning;
//!   3. the pane's PTY child dies with it. A server that exits while leaving
//!      an orphaned `sh` behind has moved the leak rather than fixed it, and
//!      that is invisible from inside the server.
//!
//! (3) is checked without `pgrep`, `/proc`, or any pid plumbing the wire
//! does not carry: the seed pane runs a shell loop that writes an
//! incrementing counter to a file every `HEARTBEAT_TICK`. After the server
//! is gone the counter is read twice, `HEARTBEAT_SETTLE` apart. A surviving
//! child keeps counting; a dead one cannot. The same file also proves the
//! pane was genuinely ALIVE at exit time, which is the entire premise —
//! without it the test would be indistinguishable from the ordinary
//! last-pane self-exit `server_self_exit.rs` already covers.
//!
//! Timing discipline: every deadline here is a HANG detector with a named
//! constant and a comment saying what the real number is. Nothing in this
//! file gates on a fork+exec finishing inside the idle interval — an earlier
//! draft did, by spacing `phux ls` calls, and it flaked on a loaded box
//! because CLI spawn latency, not the server, was what it measured.
//!
//! Harness discipline follows `rec_e2e.rs`: sockets at the root of `/tmp`
//! (macOS caps `sun_path` at 104 bytes and these run from deep worktrees),
//! pid- and counter-qualified, guard-killed and unlinked on drop.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Path to the freshly-built `phux` binary, injected by cargo.
const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// The pre-seeded session name every server here starts with.
const SESSION: &str = "work";

/// The lifetime under test, in seconds — the unit the flag takes.
///
/// NOT the CLI minimum of 1, and the reason is worth writing down because
/// the 1s version was written first and flaked. The idle clock starts when
/// the server starts, so every second of harness setup — fork/exec, bind,
/// spawning a PTY and its shell — is spent against it. At 1s a loaded box
/// could reach the deadline before the seed pane had run a single command,
/// and the failure read "the pane never started", which is a statement
/// about the machine and not about the feature. Five seconds is two orders
/// of magnitude above a healthy pane bring-up (~50ms), so setup cannot
/// plausibly consume it, while still being short enough to gate on twice.
const IDLE_SECS: u64 = 5;

/// Ceiling on "the daemon process is gone", as a HANG detector rather than a
/// timing gate.
///
/// The real number is `IDLE_SECS` plus one watchdog re-check plus process
/// teardown. This is well over twice that, per the reasoning in
/// `phux-server/tests/concurrent_attach_no_lag.rs`: under `just e2e` these
/// run alongside PTY-backed servers on a contended box, and a tight bound
/// would measure the scheduler instead of the feature. It can only elapse if
/// the server never intended to exit.
const EXIT_HANG_CEILING: Duration = Duration::from_secs(45);

/// How long a server started WITHOUT the flag is observed before it counts
/// as immortal.
///
/// A multiple of `IDLE_SECS`, because the claim is "it does not exit", not
/// "it had not exited yet". This is the guard test's whole budget and the
/// slowest thing in the file, so it is kept to the smallest multiple that
/// still makes the statement.
const NO_LIFETIME_OBSERVATION: Duration = Duration::from_secs(3 * IDLE_SECS);

/// How long `a_connected_client_postpones_the_idle_exit` holds its socket.
///
/// A multiple of `IDLE_SECS`, so a server that ignored open connections
/// would be several intervals dead by the time the assertion runs.
const HOLD_OPEN: Duration = Duration::from_secs(3 * IDLE_SECS);

/// Wait for the server to bind (cold-start bound, matching `rec_e2e.rs`).
const SOCKET_DEADLINE: Duration = Duration::from_secs(30);

/// Wait for the seed pane's first heartbeat tick.
///
/// Deliberately shorter than `SOCKET_DEADLINE`: a pane that has not run a
/// command within this window on a server whose own idle limit is
/// `IDLE_SECS` will never produce one, because the server is about to leave.
/// Failing here quickly names the real problem instead of spending half a
/// minute confirming it.
const PANE_LIVE_DEADLINE: Duration = Duration::from_secs(10);

/// Poll cadence for every wait loop in this file.
const POLL: Duration = Duration::from_millis(50);

/// Seed-pane heartbeat period. Fast enough that the post-exit sample window
/// below would catch several ticks from a survivor.
const HEARTBEAT_TICK: &str = "0.2";

/// How long to watch the heartbeat file after the server is gone.
///
/// Generously many `HEARTBEAT_TICK`s: a surviving child would advance the
/// counter several times inside it, so "unchanged" is a strong statement and
/// not a race we happened to win.
const HEARTBEAT_SETTLE: Duration = Duration::from_secs(2);

/// Monotonic counter so concurrent tests never collide on a socket path.
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A running `phux server` child plus its private socket and scratch dir.
///
/// The `Drop` kill stays even though every test here ends with the server
/// gone by design: a panicking assertion must not leak a daemon, which is
/// the failure mode this whole feature exists to prevent. Belt and braces.
struct ServerGuard {
    child: Child,
    socket: PathBuf,
    /// Owns the scratch directory `heartbeat` lives in. Never read — held
    /// solely so the directory outlives the guard rather than being unlinked
    /// the moment `start` returns.
    _dir: tempfile::TempDir,
    heartbeat: PathBuf,
    /// When the child was spawned. The idle clock starts inside the server
    /// at roughly this instant, so it is the only correct origin for "did
    /// it honour the interval?" — measuring from the end of harness setup
    /// would charge the server for time it had already spent counting.
    spawned_at: Instant,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

impl ServerGuard {
    /// Start a server whose seed pane runs a heartbeat loop forever.
    ///
    /// `idle_secs = None` starts a plain server (the historical contract);
    /// `Some(n)` adds `--exit-after-idle n`.
    fn start(idle_secs: Option<u64>) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = PathBuf::from(format!(
            "/tmp/phux-idle-e2e-{}-{n}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&socket);
        let dir = tempfile::tempdir().expect("create temp dir");
        let heartbeat = dir.path().join("heartbeat");

        // The pane's program. `$SHELL -c` wraps this (that is what
        // `--seed-command` does), so it must be POSIX-portable: no bashisms,
        // no `$'...'`. It never exits on its own, so nothing in this file
        // can be confused with the last-pane self-exit.
        let seed = format!(
            "i=0; while :; do i=$((i+1)); echo $i > {}; sleep {HEARTBEAT_TICK}; done",
            heartbeat.display()
        );

        let mut cmd = Command::new(PHUX);
        cmd.args(["server", "--session", SESSION, "--socket"])
            .arg(&socket)
            .arg("--seed-command")
            .arg(&seed);
        if let Some(secs) = idle_secs {
            cmd.arg("--exit-after-idle").arg(secs.to_string());
        }
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server");

        let guard = Self {
            child,
            socket,
            _dir: dir,
            heartbeat,
            spawned_at: Instant::now(),
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

    /// Whether the server process has already exited, right now.
    fn has_exited(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(status) => status.is_some(),
            Err(err) => panic!("try_wait on phux server: {err}"),
        }
    }

    /// Poll until the server process has exited, returning how long it took.
    /// `None` if it was still running at the deadline.
    fn wait_for_exit(&mut self, within: Duration) -> Option<Duration> {
        let start = Instant::now();
        while start.elapsed() < within {
            match self.child.try_wait() {
                Ok(Some(_status)) => return Some(start.elapsed()),
                Ok(None) => std::thread::sleep(POLL),
                Err(err) => panic!("try_wait on phux server: {err}"),
            }
        }
        None
    }

    /// The seed pane's heartbeat counter, or `None` before its first tick.
    fn heartbeat(&self) -> Option<u64> {
        let raw = std::fs::read_to_string(&self.heartbeat).ok()?;
        raw.trim().parse().ok()
    }

    /// Block until the seed pane has ticked at least once, proving the PTY
    /// child is alive and running. Every test here depends on that premise.
    fn wait_for_live_pane(&self) -> u64 {
        let deadline = Instant::now() + PANE_LIVE_DEADLINE;
        while Instant::now() < deadline {
            if let Some(count) = self.heartbeat() {
                return count;
            }
            std::thread::sleep(POLL);
        }
        panic!(
            "seed pane never wrote {} within {PANE_LIVE_DEADLINE:?} — the premise \
             of this test (a LIVE pane at exit time) does not hold. If the server \
             also has an idle lifetime, check that IDLE_SECS is still far larger \
             than pane bring-up on this machine",
            self.heartbeat.display()
        );
    }

    /// Open a bare connection to the server's socket and hand it back.
    ///
    /// No handshake, no frames — the server counts the accepted connection
    /// and that is all this needs. Retried against a deadline because the
    /// socket file existing is not the same as it being connectable.
    fn connect_raw(&self) -> UnixStream {
        let deadline = Instant::now() + SOCKET_DEADLINE;
        loop {
            match UnixStream::connect(&self.socket) {
                Ok(stream) => return stream,
                Err(err) if Instant::now() >= deadline => {
                    panic!("could not connect to {}: {err}", self.socket.display())
                }
                Err(_) => std::thread::sleep(POLL),
            }
        }
    }

    /// `phux <verb> --socket <sock> …`, asserting success. `--socket` goes
    /// right after the verb (`send-keys` uses `trailing_var_arg`).
    fn success(&self, args: &[&str]) {
        let (verb, rest) = args.split_first().expect("at least a verb");
        let out = Command::new(PHUX)
            .arg(verb)
            .arg("--socket")
            .arg(&self.socket)
            .args(rest)
            .stdin(Stdio::null())
            .output()
            .expect("run phux verb");
        assert!(
            out.status.success(),
            "phux {args:?} exited {:?}; stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The load-bearing test: a daemon nobody ever connected to exits on its
/// own, and takes its live PTY child with it.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn ephemeral_server_exits_unattended_and_reaps_its_pane() {
    let mut server = ServerGuard::start(Some(IDLE_SECS));
    let ticks_before = server.wait_for_live_pane();
    let spawned_at = server.spawned_at;

    server.wait_for_exit(EXIT_HANG_CEILING).unwrap_or_else(|| {
        panic!(
            "`phux server --exit-after-idle {IDLE_SECS}` was still running \
             {EXIT_HANG_CEILING:?} after start with no client ever connecting",
        )
    });
    let lifetime = spawned_at.elapsed();

    // The pane was alive when the server decided to go, so this really is
    // the idle lifetime firing and not the last-pane reap.
    assert!(
        ticks_before >= 1,
        "seed pane must have been running before the exit; saw {ticks_before} ticks",
    );

    // The PTY child must not have outlived its server. Sample the counter,
    // wait several heartbeat periods, sample again: a survivor advances it.
    let at_exit = server.heartbeat().expect("heartbeat file after exit");
    std::thread::sleep(HEARTBEAT_SETTLE);
    let after_settling = server.heartbeat().expect("heartbeat file after settling");
    assert_eq!(
        at_exit, after_settling,
        "the seed pane's PTY child kept running {HEARTBEAT_SETTLE:?} after the \
         server exited (counter {at_exit} -> {after_settling}); an orphaned \
         child holding a PTY is the leak this flag exists to close, only \
         harder to find",
    );

    // The interval was honoured at all, measured from the spawn — the same
    // origin the server's own clock uses. This is a floor, not a perf gate:
    // it catches "the watchdog fired immediately", which would make the flag
    // a footgun rather than a lifetime. `POLL` of slack absorbs the
    // difference between our `Instant` and the server's.
    assert!(
        lifetime >= Duration::from_secs(IDLE_SECS).saturating_sub(POLL),
        "server lived {lifetime:?}, less than its {IDLE_SECS}s idle limit",
    );
}

/// A client postpones the exit for as long as it is connected, and the
/// interval restarts from the moment it leaves — so `--exit-after-idle`
/// means "since the last client left", not "since startup".
///
/// The connection is a bare `UnixStream` that never sends a byte: no
/// `ATTACH`, no frames. That is the point. A server gating on
/// `ServerState::attached` would reap this one, and the harnesses this flag
/// is for drive their servers with one-shot control verbs that likewise
/// never attach. A `phux ls` inside the held window proves the daemon is
/// still genuinely *serving* rather than merely resident.
///
/// The clock is held open by the in-process socket rather than by a series
/// of spawned verbs, and that is a deliberate correction: the first version
/// of this test slept between `phux ls` invocations and flaked on a loaded
/// box, because a fork+exec of the CLI can take longer than `IDLE_SECS` and
/// the test then measured process-spawn latency instead of the feature.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn a_connected_client_postpones_the_idle_exit() {
    let mut server = ServerGuard::start(Some(IDLE_SECS));
    let held = server.connect_raw();
    server.wait_for_live_pane();

    // Well past the interval a never-contacted server would have died at.
    std::thread::sleep(HOLD_OPEN);
    assert!(
        !server.has_exited(),
        "server exited while a client connection was open ({HOLD_OPEN:?} held); \
         the idle clock must be disarmed for as long as anyone is connected, \
         however quiet that connection is",
    );
    // Still serving, not just still resident.
    server.success(&["ls"]);

    // Leave. The clock re-arms from here and the server follows.
    drop(held);
    server.wait_for_exit(EXIT_HANG_CEILING).unwrap_or_else(|| {
        panic!(
            "server did not exit within {EXIT_HANG_CEILING:?} after its last \
             connection closed",
        )
    });
}

/// The guard test: WITHOUT the flag, the historical contract is unchanged —
/// a server with a live pane and nobody attached stays up.
///
/// This exists so that a later change making the lifetime a default fails
/// here, loudly, instead of silently killing someone's session while they
/// are at lunch.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn without_the_flag_the_server_outlives_every_idle_window() {
    let mut server = ServerGuard::start(None);
    server.wait_for_live_pane();

    assert!(
        server.wait_for_exit(NO_LIFETIME_OBSERVATION).is_none(),
        "a `phux server` started WITHOUT --exit-after-idle exited after \
         {NO_LIFETIME_OBSERVATION:?} unattended; the multiplexer contract is \
         to live until the last pane is gone, and the idle lifetime is opt-in \
         precisely so a human's session survives them walking away",
    );

    // And it is still serving, not merely still resident.
    server.success(&["ls"]);
}
