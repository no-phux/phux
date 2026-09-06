//! Lifecycle tests for the opt-in ephemeral-server lifetime (ADR-0063,
//! `ServerConfig::exit_after_idle` / `phux server --exit-after-idle`).
//!
//! Contract, stated as the pair of tests that must both hold:
//!
//! * **With the lifetime set**, the server exits once it has been unattended
//!   for the interval — *even though a pane is still alive and running*.
//!   That is the whole point: `server_self_exit.rs` covers "the panes went
//!   away", and no lever existed for "nobody is coming back".
//! * **Without it**, nothing changes. A server with a live pane and no
//!   clients stays up forever, which is the tmux contract a human depends on
//!   when they close their laptop. The absence test is the guard against a
//!   later change making the lifetime a default.
//!
//! "Unattended" is *zero open connections*, not *zero attached clients* —
//! `connecting_disarms_the_idle_clock` pins that, because one-shot control
//! verbs (`phux ls`, `phux send-keys`) never enter `ServerState::attached`
//! and a harness driving a server purely with those must not be reaped
//! mid-script.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::time::Duration;

use phux_server::runtime::{ServerConfig, ServerRuntime};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::time::timeout;

use phux_server_testkit::{SOCKET_CONNECT_DEADLINE, run_local, wait_for_socket};

/// The idle interval every server in this file is configured with.
///
/// Short because the tests are gated on it twice over (once waiting for an
/// exit, once proving no exit), and long enough that the `LocalSet`'s startup
/// ramp — bind, seed a real PTY, spawn the actor — cannot itself consume the
/// budget on a loaded machine before the first assertion is even set up.
/// The clock arms at `SharedState::new`, i.e. before the bind, so the ramp
/// genuinely spends this budget: 200 ms keeps several multiples of headroom
/// over a ramp that measures in tens of milliseconds even on a busy 2-core
/// runner (and `wait_for_socket` connects within ~5 ms of the bind).
const IDLE_LIMIT: Duration = Duration::from_millis(200);

/// Ceiling on "the server noticed it was idle and stopped", as a HANG
/// detector rather than a timing gate.
///
/// The real number is `IDLE_LIMIT` plus one watchdog re-check plus the
/// `LocalSet` teardown — tens of milliseconds past the limit. This is two
/// orders of magnitude above that, so it can only elapse if the watchdog
/// never fired at all. Following `concurrent_attach_no_lag.rs`: anything
/// between the real number and this bound is scheduler noise on a busy box,
/// not signal, and this suite must not redden a PR for being run on a laptop
/// that is compiling something else.
const IDLE_EXIT_HANG_CEILING: Duration = Duration::from_secs(30);

/// How long the no-flag control is observed before declaring it immortal.
///
/// Must be comfortably longer than `IDLE_LIMIT` so the window genuinely
/// covers the interval a flagged server would have exited in; the multiple
/// (4x here, on top of the watchdog's 50 ms re-check floor) is what makes
/// "it did not exit" mean "it does not exit" rather than "we did not wait".
const NO_LIFETIME_OBSERVATION: Duration = Duration::from_millis(800);

/// Build a PTY-seeded server config whose seed pane runs `sh -c <script>`.
/// Mirrors `server_self_exit.rs`'s helper: a real PTY child, because the
/// claim under test is specifically that the lifetime fires *despite* one.
fn seeded_cfg(
    socket_path: std::path::PathBuf,
    script: &str,
    exit_after_idle: Option<Duration>,
) -> ServerConfig {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg(script);
    ServerConfig {
        socket_path,
        pre_seeded_session: Some("solo".to_owned()),
        seed_with_pty: true,
        seed_command: Some(cmd),
        exit_after_idle,
        ..ServerConfig::with_default_socket()
    }
}

/// A script that keeps the seed pane's child alive for far longer than any
/// deadline in this file, so every exit observed here is the idle lifetime
/// and never a reap.
const LONG_LIVED_PANE: &str = "sleep 600";

/// The load-bearing test: a server with a lifetime, a live pane, and a
/// client that never arrives, exits on its own.
///
/// Note the `pending()` shutdown future — as in `server_self_exit.rs`, the
/// ONLY way `run_async` can return here is the server deciding to stop.
/// Nothing signals it, and the pane is very much alive.
#[test]
fn idle_lifetime_exits_a_server_that_never_served_a_client() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let cfg = seeded_cfg(socket_path.clone(), LONG_LIVED_PANE, Some(IDLE_LIMIT));

        let handle = tokio::task::spawn_local(async move {
            ServerRuntime::new(cfg)
                .run_async(std::future::pending::<()>())
                .await
        });

        // Confirm the server really bound before timing anything, so a
        // failure below cannot be read as "it never started".
        let stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        // ...and drop that probe immediately: it is a live connection, and
        // holding it would disarm the very clock under test.
        drop(stream);

        let run = timeout(IDLE_EXIT_HANG_CEILING, handle)
            .await
            .expect("server with --exit-after-idle did not exit while unattended")
            .expect("server task join");
        run.expect("run_async returned an error rather than a clean idle exit");
    });
}

/// The guard against this ever becoming the default: the SAME scenario with
/// no lifetime configured must keep the multiplexer contract and stay up.
///
/// If someone later "helpfully" gives every server an idle timeout, this is
/// the test that fails, and its message says why that is wrong.
#[test]
fn without_the_flag_an_unattended_server_stays_up() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let cfg = seeded_cfg(socket_path.clone(), LONG_LIVED_PANE, None);

        let handle = tokio::task::spawn_local(async move {
            ServerRuntime::new(cfg)
                .run_async(std::future::pending::<()>())
                .await
        });

        let stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        drop(stream);

        let still_running = timeout(NO_LIFETIME_OBSERVATION, handle).await.is_err();
        assert!(
            still_running,
            "a server WITHOUT --exit-after-idle must live until its last pane is \
             gone, no matter how long nobody is attached; the idle lifetime is \
             opt-in and must never become the default",
        );
    });
}

/// An open connection disarms the clock, and closing it re-arms it.
///
/// This is what makes the flag usable by a harness that drives the server
/// with one-shot control verbs: those never attach, so gating on
/// `ServerState::attached` would reap the server between two `send-keys`
/// calls. The test holds a bare connection — no ATTACH, no frames at all —
/// for several idle intervals, then drops it and watches the server go.
#[test]
fn connecting_disarms_the_idle_clock() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let cfg = seeded_cfg(socket_path.clone(), LONG_LIVED_PANE, Some(IDLE_LIMIT));

        let handle = tokio::task::spawn_local(async move {
            ServerRuntime::new(cfg)
                .run_async(std::future::pending::<()>())
                .await
        });

        let stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        // Hold the socket open across several idle intervals. A server that
        // ignored open connections would already be gone by the end of this.
        tokio::time::sleep(IDLE_LIMIT * 4).await;
        assert!(
            !handle.is_finished(),
            "server exited while a client connection was open; the idle clock \
             must be disarmed for as long as anyone is connected, however \
             quiet that connection is",
        );

        // Now leave. The clock re-arms and the server follows.
        drop(stream);
        let run = timeout(IDLE_EXIT_HANG_CEILING, handle)
            .await
            .expect("server did not exit after its last connection closed")
            .expect("server task join");
        run.expect("run_async returned an error rather than a clean idle exit");
    });
}
