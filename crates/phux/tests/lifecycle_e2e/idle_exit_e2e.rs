//! `phux server --exit-after-idle` (ADR-0063) as a real process: the flag
//! reaches `ServerConfig`, the daemon process exits, and the pane's PTY child
//! dies with it. The seed pane writes a heartbeat counter to a file; after the
//! server exits the counter is read twice to prove the child stopped (and that
//! it was alive at exit time). Every deadline is a hang detector.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Path to the freshly-built `phux` binary, injected by cargo.
const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// The pre-seeded session name every server here starts with.
const SESSION: &str = "work";

/// The lifetime under test, in seconds. Not 1: the idle clock starts with the
/// server, and a loaded box could spend 1s on setup before the pane ran.
const IDLE_SECS: u64 = 5;

/// Hang detector for the daemon exiting (the real number is `IDLE_SECS` plus
/// a watchdog re-check and teardown).
const EXIT_HANG_CEILING: Duration = Duration::from_secs(45);

/// How long an auto-spawned server without an idle limit is observed before
/// it counts as persistent: a multiple of `IDLE_SECS`.
const NO_LIFETIME_OBSERVATION: Duration = Duration::from_secs(3 * IDLE_SECS);

/// Wait for the seed pane's first heartbeat; a pane silent this long on a
/// server about to leave never will speak.
const PANE_LIVE_DEADLINE: Duration = Duration::from_secs(10);

/// Poll cadence for every wait loop in this file.
const POLL: Duration = Duration::from_millis(50);

/// Seed-pane heartbeat period. Fast enough that the post-exit sample window
/// below would catch several ticks from a survivor.
const HEARTBEAT_TICK: &str = "0.2";

/// How long to watch the heartbeat after the server is gone: many ticks, so
/// "unchanged" is a strong statement.
const HEARTBEAT_SETTLE: Duration = Duration::from_secs(2);

/// Monotonic counter so concurrent tests never collide on a socket path.
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A running `phux server` child plus its private socket and scratch dir;
/// `Drop` still kills it so a failed assertion cannot leak a daemon.
struct ServerGuard {
    inner: common::ServerGuard,
    /// Owns the scratch directory `heartbeat` lives in. Never read — held
    /// solely so the directory outlives the guard rather than being unlinked
    /// the moment `start` returns.
    _dir: tempfile::TempDir,
    heartbeat: PathBuf,
}

impl std::ops::Deref for ServerGuard {
    type Target = common::ServerGuard;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl std::ops::DerefMut for ServerGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl ServerGuard {
    /// Start a server whose seed pane runs a heartbeat loop forever, with
    /// `--exit-after-idle` set to `idle_secs`.
    fn start(idle_secs: u64) -> Self {
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

        Self {
            inner: common::ServerGuard::builder("idle")
                .seed_command(seed)
                .idle_secs(idle_secs)
                .start(),
            _dir: dir,
            heartbeat,
        }
    }

    /// The seed pane's heartbeat counter, or `None` before its first tick.
    fn heartbeat(&self) -> Option<u64> {
        let raw = std::fs::read_to_string(&self.heartbeat).ok()?;
        raw.trim().parse().ok()
    }

    /// Block until the seed pane has ticked at least once, proving the PTY
    /// child is alive and running.
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
}

/// The load-bearing test: a daemon nobody ever connected to exits on its
/// own, and takes its live PTY child with it.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn ephemeral_server_exits_unattended_and_reaps_its_pane() {
    let mut server = ServerGuard::start(IDLE_SECS);
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

    // A floor, not a perf gate: catches a watchdog that fired immediately.
    assert!(
        lifetime >= Duration::from_secs(IDLE_SECS).saturating_sub(POLL),
        "server lived {lifetime:?}, less than its {IDLE_SECS}s idle limit",
    );
}

/// An auto-spawned daemon (a naked `phux` builds its own argv and drops the
/// `Child`), so liveness is observed through the socket, not a pid.
struct AutoSpawned {
    socket: PathBuf,
    _server: common::AutoSpawnedServer,
    /// Owns the scratch directory. Never read — held so it outlives the
    /// guard rather than being unlinked the moment `start` returns.
    _dir: tempfile::TempDir,
}

impl AutoSpawned {
    /// Trigger an auto-spawn with `phux new`, optionally setting the
    /// environment variable that gives the daemon an idle limit.
    fn start(idle_secs: Option<u64>) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = PathBuf::from(format!(
            "/tmp/phux-idle-auto-{}-{n}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&socket);
        let dir = tempfile::tempdir().expect("create temp dir");

        // `--json` because a bare `phux new` attaches, and attaching refuses
        // without a tty on both ends. The document is unread here; what
        // matters is that the verb auto-spawns a server and returns.
        let mut cmd = Command::new(PHUX);
        cmd.args(["new", "--session", SESSION, "--json", "--socket"])
            .arg(&socket);
        match idle_secs {
            Some(secs) => {
                cmd.env("PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE", secs.to_string());
            }
            // Explicitly cleared rather than merely not set: an ambient value
            // in the developer's shell would make the guard test below pass
            // for the wrong reason.
            None => {
                cmd.env_remove("PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE");
            }
        }
        let out = cmd
            .stdin(Stdio::null())
            .output()
            .expect("run phux new (auto-spawns a server)");
        assert!(
            out.status.success(),
            "phux new exited {:?}; stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr),
        );

        let mut server = common::AutoSpawnedServer::new(PHUX, socket.clone());
        server.capture_pid();
        Self {
            socket,
            _server: server,
            _dir: dir,
        }
    }

    /// Whether the socket answers right now. Costs one connection, which
    /// re-arms the idle clock: establish a premise with it, never poll.
    fn is_answering(&self) -> bool {
        UnixStream::connect(&self.socket).is_ok()
    }

    /// Poll until the socket file is gone (`None` at the deadline). Gating on
    /// the file, not a connect, is load-bearing: each connect re-arms the idle
    /// clock. Unlinking also proves the graceful shutdown path ran.
    fn wait_until_gone(&self, within: Duration) -> Option<Duration> {
        let start = Instant::now();
        while start.elapsed() < within {
            if !self.socket.exists() {
                return Some(start.elapsed());
            }
            std::thread::sleep(POLL);
        }
        None
    }
}

/// An auto-spawned daemon honours the idle limit it is given, so a suite
/// killed mid-run cannot leave an immortal daemon.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn an_auto_spawned_server_honours_the_environment_idle_limit() {
    let server = AutoSpawned::start(Some(IDLE_SECS));
    assert!(
        server.is_answering(),
        "phux new reported success but nothing is listening on the socket",
    );

    server
        .wait_until_gone(EXIT_HANG_CEILING)
        .unwrap_or_else(|| {
            panic!(
                "an auto-spawned server with PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE={IDLE_SECS} \
             was still answering {EXIT_HANG_CEILING:?} later. The variable is read by \
             the spawning process and passed to the child as --exit-after-idle, so a \
             break is either in that hand-off or in the flag itself",
            )
        });
}

/// Without the variable, auto-spawn has no lifetime: a default would end a
/// human's sessions while they were away.
#[test]
#[ignore = "spawns a real phux server; starves in the full parallel pool. Run via `just e2e`."]
fn auto_spawn_has_no_idle_limit_unless_asked() {
    let server = AutoSpawned::start(None);

    assert!(
        server.wait_until_gone(NO_LIFETIME_OBSERVATION).is_none(),
        "an auto-spawned server went away after {NO_LIFETIME_OBSERVATION:?} with no \
         PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE set; the idle lifetime is opt-in, and \
         auto-spawn is the path a human's own session runs on",
    );

    // Still serving, not merely still resident — and the one connection this
    // costs is spent after the observation window, never inside it.
    assert!(
        server.is_answering(),
        "the socket file survived but nothing is listening on it",
    );
}
